//! 端到端测试：外部设备与主时钟跟随。
//!
//! 拓扑：
//! - `Aemeath Dev In 1` → 外部输出（−30 dB），外部输出同时是主时钟；
//! - 外部输入 → `Aemeath Dev Out 1`。
//!
//! 往 Dev In 1 播放 1 kHz 正弦波，用 WASAPI 环回录下外部输出实际收到的声音，逐样本检查连续性；
//! 外部输入的内容无法预知，只确认 Dev Out 1 上有信号，稳定性看引擎日志里的桥接状态（每分钟一条）。
//!
//! 用法：`cargo run -p aemeath-engine --example external --release -- [秒数] [--output 名称] [--input 名称] [--exclusive]
//! [--output-rate 采样率] [--resampler poly|sinc|fft]`
//! 默认两台外部设备都以共享方式打开（为了环回检查）；`--exclusive` 时独占打开，不做环回检查。
//! `--output-rate` 让外部输出独占并固定在这个采样率（高采样率断音的复现），外部输入不受影响。
//! 不指定名称时用系统默认的播放和录音设备。外部输出会发出很轻的 1 kHz 提示音。agent 执行时不超过 5 分钟。

mod common;

use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

use aemeath_engine::{Engine, EngineConfig, EngineEvent};
use aemeath_io::IoDirection;
use aemeath_protocol::ids::{NodeId, SlotDirection, TopologyRevision};
use aemeath_protocol::message::EndpointStatus;
use aemeath_protocol::topology::{
    BufferChoice, DeviceAccess, DeviceSettings, FftPreset, FormatChoice, NodeKind, PolyPreset, Resampler,
    RouteTopology, SincPreset, Strip, TopologyNode, TopologyRoute,
};
use aemeath_protocol::units::{GainDb, SampleRate};
use aemeath_usbip::identity::IdentityProfile;
use common::{
    Attachment, PORT, Result, capture_peak, capture_sine, endpoint_id, find_device, join, render_sine, slot, timed,
};

const RENDER_SLOT: &str = "Aemeath Dev In 1";
const CAPTURE_SLOT: &str = "Aemeath Dev Out 1";
const BUS_IDS: [&str; 2] = ["1-1", "1-9"];
const AMPLITUDE: f64 = 0.5;
/// 送往外部输出的增益：只发出很轻的提示音
const OUTPUT_GAIN_DB: f32 = -30.0;
const TAIL_SECONDS: f64 = 1.0;
/// 信号出现后跳过的时间，外加外部输出开流前攒水位、主时钟跟随启动的时间
const SETTLE_SECONDS: f64 = 1.5;
const ENGINE_RATE: u32 = 48_000;
const DECIBELS_PER_DECADE: f64 = 20.0;

struct Options {
    seconds: f64,
    output: Option<String>,
    input: Option<String>,
    /// 两台外部设备都以独占方式打开。独占的流不经过 Windows 混音器，环回录不到，
    /// 所以这时不做环回检查，只检查两台设备都在线、没有离线
    exclusive: bool,
    output_rate: Option<u32>,
    resampler: Resampler,
}

fn parse_resampler(name: Option<String>) -> Result<Resampler> {
    match name.as_deref() {
        Some("poly") => Ok(Resampler::PolyAsync {
            preset: PolyPreset::default(),
        }),
        Some("sinc") => Ok(Resampler::SincAsync {
            preset: SincPreset::default(),
        }),
        Some("fft") => Ok(Resampler::FftSync {
            preset: FftPreset::default(),
        }),
        other => Err(format!("unknown resampler: {other:?}").into()),
    }
}

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

fn parse() -> Result<Options> {
    let mut options = Options {
        seconds: 120.0,
        output: None,
        input: None,
        exclusive: false,
        output_rate: None,
        resampler: Resampler::default(),
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output" => options.output = args.next(),
            "--input" => options.input = args.next(),
            "--exclusive" => options.exclusive = true,
            "--output-rate" => options.output_rate = args.next().map(|rate| rate.parse()).transpose()?,
            "--resampler" => options.resampler = parse_resampler(args.next())?,
            seconds => options.seconds = seconds.parse()?,
        }
    }
    Ok(options)
}

fn run() -> Result<bool> {
    tracing_subscriber::fmt().with_max_level(tracing::Level::INFO).init();
    let options = parse()?;
    wasapi::initialize_mta().ok()?;
    let output_device = find_device(wasapi::Direction::Render, options.output.as_deref())?;
    let input_device = find_device(wasapi::Direction::Capture, options.input.as_deref())?;
    let output_name = output_device.get_friendlyname()?;
    let input_name = input_device.get_friendlyname()?;
    println!("external output (clock master): {output_name}");
    println!("external input: {input_name}");

    let mut engine = Engine::start(EngineConfig {
        usbip_port: PORT,
        identity: IdentityProfile::Development,
    })?;
    let settings = DeviceSettings {
        access: if options.exclusive {
            DeviceAccess::default()
        } else {
            DeviceAccess::Shared
        },
        resampler: None,
    };
    let output_settings = match options.output_rate {
        Some(rate) => {
            let capabilities = aemeath_io::capabilities::probe_device(&output_device)?;
            println!("exclusive formats: {:?}", capabilities.exclusive_formats);
            let format = capabilities
                .recommended(SampleRate::new(rate)?)
                .filter(|format| format.sample_rate.hz() == rate)
                .ok_or_else(|| format!("{output_name} has no exclusive format at {rate} Hz"))?;
            println!("exclusive output format: {format:?}");
            DeviceSettings {
                access: DeviceAccess::Exclusive {
                    format: FormatChoice::Fixed { format },
                    buffer: BufferChoice::default(),
                },
                resampler: None,
            }
        }
        None => settings,
    };
    engine.apply_topology(&topology(
        endpoint_id(&output_device)?,
        endpoint_id(&input_device)?,
        output_settings,
        settings,
        options.resampler,
    )?)?;
    let _attachment = Attachment::attach(PORT, &BUS_IDS);
    let started = Instant::now();

    let seconds = options.seconds;
    let expected = AMPLITUDE * 10f64.powf(f64::from(OUTPUT_GAIN_DB) / DECIBELS_PER_DECADE);
    let window = seconds - TAIL_SECONDS - SETTLE_SECONDS;
    let output_for_loopback = options.output.clone();
    let exclusive = options.exclusive || options.output_rate.is_some();
    let loopback = thread::spawn(move || -> Result<Option<common::Analysis>> {
        if exclusive {
            return Ok(None);
        }
        wasapi::initialize_mta().ok()?;
        let device = find_device(wasapi::Direction::Render, output_for_loopback.as_deref())?;
        timed(started, capture_sine(&device, seconds + TAIL_SECONDS, window, expected)).map(Some)
    });
    let input_path = thread::spawn(move || -> Result<f64> {
        wasapi::initialize_mta().ok()?;
        let device = find_device(wasapi::Direction::Capture, Some(CAPTURE_SLOT))?;
        timed(started, capture_peak(&device, seconds))
    });
    thread::sleep(Duration::from_millis(500));
    let render = thread::spawn(move || timed(started, render_sine(RENDER_SLOT, seconds, AMPLITUDE)));
    let rendered = join(&format!("rendering into {RENDER_SLOT}"), render)?;
    let analysis = join(&format!("loopback capture of {output_name}"), loopback)?;
    let input_peak = join(&format!("capture from {CAPTURE_SLOT}"), input_path)?;

    // 记录两台外部设备是否上线过、是否离线过
    let mut online = (false, false);
    let mut went_offline = false;
    while let Some(event) = engine.next_event(Duration::ZERO) {
        match event {
            EngineEvent::Endpoint(event) => {
                println!("endpoint {:?}: {:?}", event.direction, event.status);
                match (&event.status, event.direction) {
                    (EndpointStatus::Online { .. }, IoDirection::Output) => online.0 = true,
                    (EndpointStatus::Online { .. }, IoDirection::Input) => online.1 = true,
                    (EndpointStatus::Offline { .. }, _) => went_offline = true,
                    (EndpointStatus::NotRunning | EndpointStatus::ProgramMissing, _) => {}
                }
            }
            EngineEvent::AppSource(_) | EngineEvent::Recorder { .. } => {}
            EngineEvent::SlotFormat { slot, format } => println!("slot {slot} format: {format:?}"),
            EngineEvent::SlotConnection { slot, connected } => println!("slot {slot} connected: {connected}"),
        }
    }
    drop(_attachment);
    drop(engine);

    println!("rendered {rendered:.1} s into {RENDER_SLOT}");
    println!("{input_name} → {CAPTURE_SLOT}: peak {input_peak:.5}");
    let passed = match analysis {
        Some(analysis) => {
            println!(
                "loopback of {output_name} at {} Hz: checked {:.1} s, peak {:.4} (expected {expected:.4}), glitches {}, discontinuity flags {}",
                analysis.sample_rate,
                analysis.checked_seconds(),
                analysis.peak,
                analysis.glitches,
                analysis.discontinuities
            );
            (analysis.peak - expected).abs() < expected * 0.1
                && analysis.checked_seconds() >= window * 0.95
                && analysis.glitches == 0
        }
        None => {
            println!("exclusive mode: both devices online {online:?}, went offline {went_offline}");
            online == (true, true) && !went_offline
        }
    };
    println!("{}", if passed { "PASS" } else { "FAIL" });
    Ok(passed)
}

fn topology(
    output: aemeath_protocol::ids::EndpointId,
    input: aemeath_protocol::ids::EndpointId,
    output_settings: DeviceSettings,
    input_settings: DeviceSettings,
    resampler: Resampler,
) -> Result<RouteTopology> {
    let slot_input = NodeId(1);
    let external_output = NodeId(2);
    let external_input = NodeId(3);
    let slot_output = NodeId(4);
    let node = |id, kind: NodeKind| TopologyNode {
        id,
        slot_settings: None,
        io: kind.is_external().then_some(if id == external_output {
            output_settings
        } else {
            input_settings
        }),
        kind,
        strip: Strip::default(),
    };
    Ok(RouteTopology {
        revision: TopologyRevision(1),
        engine_sample_rate: SampleRate::new(ENGINE_RATE)?,
        clock_master: Some(external_output),
        resampler,
        nodes: vec![
            node(
                slot_input,
                NodeKind::VirtualInput {
                    slot: slot(SlotDirection::In, 1)?,
                },
            ),
            node(external_output, NodeKind::ExternalOutput { device: output }),
            node(external_input, NodeKind::ExternalInput { device: input }),
            node(
                slot_output,
                NodeKind::VirtualOutput {
                    slot: slot(SlotDirection::Out, 1)?,
                },
            ),
        ],
        routes: vec![
            TopologyRoute {
                from: slot_input,
                to: external_output,
                gain: GainDb::new(OUTPUT_GAIN_DB)?,
                enabled: true,
            },
            TopologyRoute {
                from: external_input,
                to: slot_output,
                gain: GainDb::UNITY,
                enabled: true,
            },
        ],
    })
}
