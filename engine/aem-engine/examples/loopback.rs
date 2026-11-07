//! 端到端测试：`Aemeath Dev In 1` → 引擎 → `Aemeath Dev Out 1`。
//!
//! 启动引擎并下发计划，用 `usbip.exe` 挂载两个开发槽位，然后像普通应用一样：
//! 往 Dev In 1 播放 1 kHz 正弦波（WASAPI 共享模式），同时从 Dev Out 1 录音，检查电平和连续性。
//! 连续性用正弦递推 x[n] = 2cos(ω)·x[n-1] − x[n-2] 检查：任何丢帧、重复或跳变都会让残差突增。
//!
//! 用法：`cargo run -p aem-engine --example loopback --release -- [秒数] [引擎采样率]`
//! 只使用开发槽位（ADR-0009），挂载不需要管理员权限；结束时卸下。

use std::error::Error;
use std::f64::consts::TAU;
use std::process::{Command, ExitCode};
use std::thread;
use std::time::{Duration, Instant};

use aem_engine::{Engine, EngineConfig, EngineEvent};
use aem_protocol::ids::{NodeId, PlanRevision, SlotDirection, SlotId, SlotNumber};
use aem_protocol::plan::{NodeKind, PlanNode, PlanRoute, Resampler, RoutePlan, Strip};
use aem_protocol::units::{GainDb, SampleRate};
use aem_usbip::identity::IdentityProfile;
use wasapi::{DeviceEnumerator, SampleType, StreamMode};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

const USBIP: &str = r"C:\Program Files\USBip\usbip.exe";
/// 避开 3241 上残留的后台重连
const PORT: u16 = 3242;
const RENDER_DEVICE: &str = "Aemeath Dev In 1";
const CAPTURE_DEVICE: &str = "Aemeath Dev Out 1";
const BUS_IDS: [&str; 2] = ["1-1", "1-9"];
const SINE_HZ: f64 = 1_000.0;
const AMPLITUDE: f64 = 0.5;
/// 正弦递推残差超过这个值算一次中断（f32 直通时残差约 1e-6，16 位量化约 1e-4）
const GLITCH_THRESHOLD: f64 = 0.02;
/// 一次中断之后这么多个样本内的残差不重复计数
const GLITCH_HOLDOFF: u64 = 64;
/// 信号出现后先跳过这段再开始检查；播放结束前留出这段不检查
const SETTLE_SECONDS: f64 = 0.3;
const TAIL_SECONDS: f64 = 1.0;
const SIGNAL_PRESENT: f64 = 0.05;
const DEVICE_TIMEOUT: Duration = Duration::from_secs(60);
const EVENT_TIMEOUT_MS: u32 = 2_000;

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<bool> {
    tracing_subscriber::fmt().with_max_level(tracing::Level::INFO).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let seconds: f64 = args.first().map(|text| text.parse()).transpose()?.unwrap_or(20.0);
    let rate: u32 = args.get(1).map(|text| text.parse()).transpose()?.unwrap_or(48_000);

    let mut engine = Engine::start(EngineConfig {
        usbip_port: PORT,
        identity: IdentityProfile::Development,
    })?;
    engine.apply_plan(&loopback_plan(SampleRate::new(rate)?)?)?;
    for busid in BUS_IDS {
        attach(busid);
    }

    let capture = thread::spawn(move || capture(CAPTURE_DEVICE, seconds + TAIL_SECONDS));
    thread::sleep(Duration::from_millis(500));
    let render = thread::spawn(move || render(RENDER_DEVICE, seconds));
    let rendered = render.join().map_err(|_| "render thread panicked")??;
    let analysis = capture.join().map_err(|_| "capture thread panicked")??;

    while let Some(event) = engine.next_event(Duration::ZERO) {
        match event {
            EngineEvent::SlotFormat { slot, format } => println!("slot {slot} format: {format:?}"),
            EngineEvent::SlotConnection { slot, connected } => println!("slot {slot} connected: {connected}"),
        }
    }
    detach_all();
    drop(engine);

    println!("rendered {rendered:.1} s into {RENDER_DEVICE}");
    println!(
        "captured from {CAPTURE_DEVICE} at {} Hz: checked {:.1} s, peak {:.3}, glitches {}, discontinuity flags {}",
        analysis.sample_rate,
        analysis.checked as f64 / f64::from(analysis.sample_rate),
        analysis.peak,
        analysis.glitches,
        analysis.discontinuities
    );
    let level_ok = (analysis.peak - AMPLITUDE).abs() < 0.05;
    let long_enough =
        analysis.checked as f64 >= (seconds - SETTLE_SECONDS - TAIL_SECONDS - 1.0) * f64::from(analysis.sample_rate);
    let passed = level_ok && long_enough && analysis.glitches == 0;
    println!("{}", if passed { "PASS" } else { "FAIL" });
    Ok(passed)
}

fn slot(direction: SlotDirection, number: u8) -> Result<SlotId> {
    Ok(SlotId {
        direction,
        number: SlotNumber::new(number)?,
    })
}

fn loopback_plan(rate: SampleRate) -> Result<RoutePlan> {
    let input = NodeId(1);
    let output = NodeId(2);
    Ok(RoutePlan {
        revision: PlanRevision(1),
        engine_sample_rate: rate,
        clock_master: None,
        resampler: Resampler::default(),
        nodes: vec![
            PlanNode {
                id: input,
                kind: NodeKind::VirtualInput {
                    slot: slot(SlotDirection::In, 1)?,
                },
                strip: Strip::default(),
            },
            PlanNode {
                id: output,
                kind: NodeKind::VirtualOutput {
                    slot: slot(SlotDirection::Out, 1)?,
                },
                strip: Strip::default(),
            },
        ],
        routes: vec![PlanRoute {
            from: input,
            to: output,
            gain: GainDb::UNITY,
            enabled: true,
        }],
    })
}

fn attach(busid: &str) {
    let port = PORT.to_string();
    let output = Command::new(USBIP)
        .args(["-t", &port, "attach", "-r", "127.0.0.1", "-b", busid, "--once"])
        .output();
    match output {
        Ok(output) => println!("attach {busid}: {}", String::from_utf8_lossy(&output.stdout).trim()),
        Err(error) => println!("attach {busid}: cannot run usbip.exe: {error}"),
    }
}

/// 卸下所有指向本测试端口的挂载。
fn detach_all() {
    let Ok(output) = Command::new(USBIP).arg("port").output() else {
        return;
    };
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    let needle = format!("127.0.0.1:{PORT}/");
    let mut current: Option<String> = None;
    for line in text.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("Port ") {
            current = rest
                .split(':')
                .next()
                .map(|port| port.trim().trim_start_matches('0').to_owned());
        } else if line.contains(&needle)
            && let Some(port) = current.take()
        {
            let port = if port.is_empty() { "0".to_owned() } else { port };
            let _ = Command::new(USBIP).args(["detach", "-p", &port]).output();
            println!("detached port {port}");
        }
    }
}

fn find_device(direction: wasapi::Direction, name: &str) -> Result<wasapi::Device> {
    let deadline = Instant::now() + DEVICE_TIMEOUT;
    loop {
        let enumerator = DeviceEnumerator::new()?;
        for device in &enumerator.get_device_collection(direction)? {
            let device = device?;
            if device.get_friendlyname().is_ok_and(|friendly| friendly.contains(name)) {
                return Ok(device);
            }
        }
        if Instant::now() > deadline {
            return Err(format!("device {name} did not appear").into());
        }
        thread::sleep(Duration::from_millis(500));
    }
}

/// 共享模式的混音格式一般是 32 位浮点；这个测试只支持这种格式。
fn require_float32(format: &wasapi::WaveFormat) -> Result<()> {
    if matches!(format.get_subformat()?, SampleType::Float) && format.get_bitspersample() == 32 {
        Ok(())
    } else {
        Err("the shared-mode mix format is not 32-bit float".into())
    }
}

fn render(name: &str, seconds: f64) -> Result<f64> {
    wasapi::initialize_mta().ok()?;
    let device = find_device(wasapi::Direction::Render, name)?;
    let mut client = device.get_iaudioclient()?;
    let format = client.get_mixformat()?;
    require_float32(&format)?;
    let (default_period, _) = client.get_device_period()?;
    client.initialize_client(
        &format,
        wasapi::Direction::Render,
        StreamMode::EventsShared {
            autoconvert: false,
            buffer_duration_hns: default_period,
        },
        None,
    )?;
    let event = client.set_get_eventhandle()?;
    let render_client = client.get_audiorenderclient()?;
    let channels = usize::from(format.get_nchannels());
    let rate = f64::from(format.get_samplespersec());
    let total_frames = (seconds * rate) as u64;
    let mut written = 0u64;
    let mut bytes = Vec::new();

    let fill = |frames: usize, written: &mut u64, bytes: &mut Vec<u8>| {
        bytes.clear();
        for _ in 0..frames {
            let sample = if *written < total_frames {
                (AMPLITUDE * (TAU * SINE_HZ * *written as f64 / rate).sin()) as f32
            } else {
                0.0
            };
            for _ in 0..channels {
                bytes.extend_from_slice(&sample.to_le_bytes());
            }
            *written += 1;
        }
    };

    fill(
        client.get_available_space_in_frames()? as usize,
        &mut written,
        &mut bytes,
    );
    render_client.write_to_device(&bytes, None)?;
    client.start_stream()?;
    while written < total_frames {
        event.wait_for_event(EVENT_TIMEOUT_MS)?;
        fill(
            client.get_available_space_in_frames()? as usize,
            &mut written,
            &mut bytes,
        );
        render_client.write_to_device(&bytes, None)?;
    }
    client.stop_stream()?;
    Ok(written as f64 / rate)
}

struct Analysis {
    sample_rate: u32,
    checked: u64,
    peak: f64,
    glitches: u64,
    discontinuities: u64,
}

fn capture(name: &str, seconds: f64) -> Result<Analysis> {
    wasapi::initialize_mta().ok()?;
    let device = find_device(wasapi::Direction::Capture, name)?;
    let mut client = device.get_iaudioclient()?;
    let format = client.get_mixformat()?;
    require_float32(&format)?;
    let (default_period, _) = client.get_device_period()?;
    client.initialize_client(
        &format,
        wasapi::Direction::Capture,
        StreamMode::EventsShared {
            autoconvert: false,
            buffer_duration_hns: default_period,
        },
        None,
    )?;
    let event = client.set_get_eventhandle()?;
    let capture_client = client.get_audiocaptureclient()?;
    let block_align = format.get_blockalign() as usize;
    let sample_rate = format.get_samplespersec();
    let rate = f64::from(sample_rate);
    let coefficient = 2.0 * (TAU * SINE_HZ / rate).cos();

    let mut analysis = Analysis {
        sample_rate,
        checked: 0,
        peak: 0.0,
        glitches: 0,
        discontinuities: 0,
    };
    let (mut previous, mut before_previous) = (0.0f64, 0.0f64);
    let mut index = 0u64;
    let mut signal_start: Option<u64> = None;
    let mut last_glitch: Option<u64> = None;
    let settle = (SETTLE_SECONDS * rate) as u64;
    // 检查窗口：信号出现后跳过 settle，到播放结束前 TAIL_SECONDS 为止
    let window = ((seconds - TAIL_SECONDS - TAIL_SECONDS) * rate) as u64;
    let mut buffer = Vec::new();

    client.start_stream()?;
    let started = Instant::now();
    while started.elapsed().as_secs_f64() < seconds {
        event.wait_for_event(EVENT_TIMEOUT_MS)?;
        while let Some(frames) = capture_client.get_next_packet_size()?.filter(|frames| *frames > 0) {
            buffer.resize(frames as usize * block_align, 0);
            let (read, info) = capture_client.read_from_device(&mut buffer)?;
            if info.flags.data_discontinuity {
                analysis.discontinuities += 1;
            }
            for frame in buffer[..read as usize * block_align].chunks_exact(block_align) {
                let sample = if info.flags.silent {
                    0.0
                } else {
                    f64::from(f32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]))
                };
                if signal_start.is_none() && sample.abs() > SIGNAL_PRESENT {
                    signal_start = Some(index);
                }
                if let Some(start) = signal_start {
                    let offset = index - start;
                    if offset >= settle && offset < settle + window {
                        analysis.checked += 1;
                        analysis.peak = analysis.peak.max(sample.abs());
                        let residual = sample - (coefficient * previous - before_previous);
                        let in_holdoff = last_glitch.is_some_and(|last| index - last < GLITCH_HOLDOFF);
                        if residual.abs() > GLITCH_THRESHOLD && !in_holdoff {
                            analysis.glitches += 1;
                            if analysis.glitches <= 5 {
                                println!("glitch at {:.3} s, residual {residual:.4}", index as f64 / rate);
                            }
                            last_glitch = Some(index);
                        }
                    }
                }
                before_previous = previous;
                previous = sample;
                index += 1;
            }
        }
    }
    client.stop_stream()?;
    Ok(analysis)
}
