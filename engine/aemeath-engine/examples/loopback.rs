//! 端到端测试：`Aemeath Dev In 1` → 引擎 → `Aemeath Dev Out 1`。
//!
//! 启动引擎并下发拓扑，用 `usbip.exe` 挂载两个开发槽位，然后像普通应用一样：
//! 往 Dev In 1 播放 1 kHz 正弦波（WASAPI 共享模式），同时从 Dev Out 1 录音，逐样本检查电平和连续性。
//!
//! 用法：`cargo run -p aemeath-engine --example loopback --release -- [秒数] [引擎采样率]`
//! 只使用开发槽位（ADR-0009），挂载不需要管理员权限；结束时卸下。agent 执行时不超过 5 分钟。

mod common;

use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

use aemeath_engine::{Engine, EngineConfig, EngineEvent};
use aemeath_protocol::ids::{NodeId, SlotDirection, TopologyRevision};
use aemeath_protocol::topology::{NodeKind, Resampler, RouteTopology, Strip, TopologyNode, TopologyRoute};
use aemeath_protocol::units::{GainDb, SampleRate};
use aemeath_usbip::identity::IdentityProfile;
use common::{Attachment, PORT, Result, capture_sine, find_device, join, render_sine, slot, timed};

const RENDER_DEVICE: &str = "Aemeath Dev In 1";
const CAPTURE_DEVICE: &str = "Aemeath Dev Out 1";
const BUS_IDS: [&str; 2] = ["1-1", "1-9"];
const AMPLITUDE: f64 = 0.5;
/// 录音比播放多录这么久；检查窗口在播放结束前留出同样的余量
const TAIL_SECONDS: f64 = 1.0;
const SETTLE_SECONDS: f64 = 0.5;

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
    engine.apply_topology(&topology(SampleRate::new(rate)?)?)?;
    let _attachment = Attachment::attach(PORT, &BUS_IDS);
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

fn topology(rate: SampleRate) -> Result<RouteTopology> {
    let input = NodeId(1);
    let output = NodeId(2);
    Ok(RouteTopology {
        revision: TopologyRevision(1),
        engine_sample_rate: rate,
        clock_master: None,
        resampler: Resampler::default(),
        nodes: vec![
            TopologyNode {
                id: input,
                kind: NodeKind::VirtualInput {
                    slot: slot(SlotDirection::In, 1)?,
                },
                strip: Strip::default(),
            },
            TopologyNode {
                id: output,
                kind: NodeKind::VirtualOutput {
                    slot: slot(SlotDirection::Out, 1)?,
                },
                strip: Strip::default(),
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
