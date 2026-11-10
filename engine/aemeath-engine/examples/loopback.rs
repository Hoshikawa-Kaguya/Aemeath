//! 端到端测试：`Aemeath Dev In 1` → 引擎 → `Aemeath Dev Out 1`。
//!
//! 启动引擎并下发拓扑，用 `usbip.exe` 挂载两个开发槽位，然后像普通应用一样：
//! 往 Dev In 1 播放 1 kHz 正弦波（WASAPI 共享模式），同时从 Dev Out 1 录音，逐样本检查电平和连续性。
//!
//! 用法：`cargo run -p aemeath-engine --example loopback --release -- [秒数] [引擎采样率]
//! [--in 采样率:样本格式] [--out 采样率:样本格式] [--resampler poly|sinc|fft] [--format-matrix]`
//! `--in`、`--out` 把两个槽位设成指定的设备格式，验证固定比率重采样（M4 计划 E2E 第 2 项）。
//! `--format-matrix` 只逐项设置并读回全部采样率和样本格式（第 3 项）。结束后用 slot-maintenance 把格式设回去。
//! 只使用开发槽位（ADR-0009），挂载不需要管理员权限；结束时卸下。agent 执行时不超过 5 分钟。

mod common;

use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

use aemeath_engine::{Engine, EngineConfig, EngineEvent};
use aemeath_protocol::ids::{NodeId, SlotDirection, TopologyRevision};
use aemeath_protocol::message::{DeviceFormat, SampleFormat};
use aemeath_protocol::topology::{
    FftPreset, NodeKind, PolyPreset, Resampler, RouteTopology, SincPreset, Strip, TopologyNode, TopologyRoute,
};
use aemeath_protocol::units::{GainDb, SampleRate};
use aemeath_sys::policy_config::PolicyConfig;
use aemeath_usbip::identity::IdentityProfile;
use common::{Attachment, PORT, Result, capture_sine, endpoint_id, find_device, join, render_sine, slot, timed};

const RENDER_DEVICE: &str = "Aemeath Dev In 1";
const CAPTURE_DEVICE: &str = "Aemeath Dev Out 1";
const BUS_IDS: [&str; 2] = ["1-1", "1-9"];
const AMPLITUDE: f64 = 0.5;
/// 录音比播放多录这么久；检查窗口在播放结束前留出同样的余量
const TAIL_SECONDS: f64 = 1.0;
const SETTLE_SECONDS: f64 = 0.5;
const SLOT_CHANNELS: u16 = 2;
/// 共享模式下虚拟设备可用的采样率和全部样本格式（M4 计划 F3）
const MATRIX_RATES: [u32; 8] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000];
const MATRIX_FORMATS: [SampleFormat; 5] = [
    SampleFormat::Int16,
    SampleFormat::Int24,
    SampleFormat::Int24In32,
    SampleFormat::Int32,
    SampleFormat::Float32,
];

fn parse_format(text: &str) -> Result<(u32, SampleFormat)> {
    let (rate, format) = text.split_once(':').ok_or("expected rate:format")?;
    let format = match format {
        "int16" => SampleFormat::Int16,
        "int24" => SampleFormat::Int24,
        "int24in32" => SampleFormat::Int24In32,
        "int32" => SampleFormat::Int32,
        "float32" => SampleFormat::Float32,
        other => return Err(format!("unknown sample format {other}").into()),
    };
    Ok((rate.parse()?, format))
}

fn set_format(policy: &PolicyConfig, id: &str, rate: u32, sample_format: SampleFormat) -> Result<bool> {
    let wanted = DeviceFormat {
        sample_rate: SampleRate::new(rate)?,
        sample_format,
        channels: SLOT_CHANNELS,
    };
    policy.set_device_format(id, wanted)?;
    Ok(policy.device_format(id)? == wanted)
}

fn format_matrix() -> Result<bool> {
    let policy = PolicyConfig::new()?;
    let mut failures = 0;
    for (direction, name) in [
        (wasapi::Direction::Render, RENDER_DEVICE),
        (wasapi::Direction::Capture, CAPTURE_DEVICE),
    ] {
        let id = endpoint_id(&find_device(direction, Some(name))?)?;
        for rate in MATRIX_RATES {
            for sample_format in MATRIX_FORMATS {
                let matched = set_format(&policy, &id.0, rate, sample_format).unwrap_or(false);
                if !matched {
                    failures += 1;
                    println!("FAILED {name} {rate} {sample_format:?}");
                }
            }
        }
    }
    let total = 2 * MATRIX_RATES.len() * MATRIX_FORMATS.len();
    println!("format matrix: {} of {total} set and read back", total - failures);
    Ok(failures == 0)
}

fn parse_resampler(name: &str) -> Result<Resampler> {
    match name {
        "poly" => Ok(Resampler::PolyAsync {
            preset: PolyPreset::default(),
        }),
        "sinc" => Ok(Resampler::SincAsync {
            preset: SincPreset::default(),
        }),
        "fft" => Ok(Resampler::FftSync {
            preset: FftPreset::default(),
        }),
        other => Err(format!("unknown resampler {other}").into()),
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

fn run() -> Result<bool> {
    tracing_subscriber::fmt().with_max_level(tracing::Level::INFO).init();
    let mut positional = Vec::new();
    let mut in_format = None;
    let mut out_format = None;
    let mut resampler = Resampler::default();
    let mut matrix = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--in" => in_format = Some(parse_format(&args.next().ok_or("--in needs a value")?)?),
            "--out" => out_format = Some(parse_format(&args.next().ok_or("--out needs a value")?)?),
            "--resampler" => resampler = parse_resampler(&args.next().ok_or("--resampler needs a value")?)?,
            "--format-matrix" => matrix = true,
            _ => positional.push(arg),
        }
    }
    let seconds: f64 = positional.first().map(|text| text.parse()).transpose()?.unwrap_or(20.0);
    let rate: u32 = positional
        .get(1)
        .map(|text| text.parse())
        .transpose()?
        .unwrap_or(48_000);
    wasapi::initialize_mta().ok()?;

    let mut engine = Engine::start(EngineConfig {
        usbip_port: PORT,
        identity: IdentityProfile::Development,
    })?;
    engine.apply_topology(&topology(SampleRate::new(rate)?, resampler)?)?;
    let _attachment = Attachment::attach(PORT, &BUS_IDS);
    if matrix {
        let passed = format_matrix()?;
        println!("{}", if passed { "PASS" } else { "FAIL" });
        return Ok(passed);
    }
    let policy = PolicyConfig::new()?;
    for (direction, name, format) in [
        (wasapi::Direction::Render, RENDER_DEVICE, in_format),
        (wasapi::Direction::Capture, CAPTURE_DEVICE, out_format),
    ] {
        if let Some((slot_rate, sample_format)) = format {
            let id = endpoint_id(&find_device(direction, Some(name))?)?;
            let matched = set_format(&policy, &id.0, slot_rate, sample_format)?;
            println!("{name}: {slot_rate} {sample_format:?}, read back: {matched}");
        }
    }
    // 改格式后端点上的流要重建，等它稳定
    thread::sleep(Duration::from_secs(1));
    let started = Instant::now();

    let window = seconds - TAIL_SECONDS - SETTLE_SECONDS;
    let capture = thread::spawn(move || -> Result<common::Analysis> {
        wasapi::initialize_mta().ok()?;
        let device = find_device(wasapi::Direction::Capture, Some(CAPTURE_DEVICE))?;
        timed(
            started,
            capture_sine(&device, seconds + TAIL_SECONDS, window, AMPLITUDE),
        )
    });
    thread::sleep(Duration::from_millis(500));
    let render = thread::spawn(move || timed(started, render_sine(RENDER_DEVICE, seconds, AMPLITUDE)));
    let rendered = join(&format!("rendering into {RENDER_DEVICE}"), render)?;
    let analysis = join(&format!("capture from {CAPTURE_DEVICE}"), capture)?;

    while let Some(event) = engine.next_event(Duration::ZERO) {
        match event {
            EngineEvent::SlotFormat { slot, format } => println!("slot {slot} format: {format:?}"),
            EngineEvent::SlotConnection { slot, connected } => println!("slot {slot} connected: {connected}"),
            EngineEvent::Endpoint(event) => println!("endpoint: {event:?}"),
            EngineEvent::AppSource(event) => println!("app source: {event:?}"),
            EngineEvent::Recorder { node, state } => println!("{node}: {state:?}"),
        }
    }
    drop(_attachment);
    drop(engine);

    println!("rendered {rendered:.1} s into {RENDER_DEVICE}");
    println!(
        "captured from {CAPTURE_DEVICE} at {} Hz: checked {:.1} s, peak {:.3}, glitches {}, discontinuity flags {}",
        analysis.sample_rate,
        analysis.checked_seconds(),
        analysis.peak,
        analysis.glitches,
        analysis.discontinuities
    );
    let passed = (analysis.peak - AMPLITUDE).abs() < 0.05
        && analysis.checked_seconds() >= window * 0.95
        && analysis.glitches == 0;
    println!("{}", if passed { "PASS" } else { "FAIL" });
    Ok(passed)
}

fn topology(rate: SampleRate, resampler: Resampler) -> Result<RouteTopology> {
    let input = NodeId(1);
    let output = NodeId(2);
    Ok(RouteTopology {
        revision: TopologyRevision(1),
        engine_sample_rate: rate,
        clock_master: None,
        resampler,
        nodes: vec![
            TopologyNode {
                id: input,
                kind: NodeKind::VirtualInput {
                    slot: slot(SlotDirection::In, 1)?,
                },
                strip: Strip::default(),
                slot_settings: None,
                io: None,
            },
            TopologyNode {
                id: output,
                kind: NodeKind::VirtualOutput {
                    slot: slot(SlotDirection::Out, 1)?,
                },
                strip: Strip::default(),
                slot_settings: None,
                io: None,
            },
        ],
        routes: vec![TopologyRoute {
            from: input,
            to: output,
            gain: GainDb::UNITY,
            enabled: true,
        }],
    })
}
