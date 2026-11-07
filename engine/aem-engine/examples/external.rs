//! 端到端测试：外部设备与主时钟跟随。
//!
//! 计划：
//! - `Aemeath Dev In 1` → 外部输出（−30 dB），外部输出同时是主时钟；
//! - 外部输入 → `Aemeath Dev Out 1`。
//!
//! 往 Dev In 1 播放 1 kHz 正弦波，用 WASAPI 环回录下外部输出实际收到的声音，逐样本检查连续性；
//! 外部输入的内容无法预知，只确认 Dev Out 1 上有信号，稳定性看引擎日志里的桥接状态（每分钟一条）。
//!
//! 用法：`cargo run -p aem-engine --example external --release -- [秒数] [--output 名称] [--input 名称]`
//! 不指定名称时用系统默认的播放和录音设备。外部输出会发出很轻的 1 kHz 提示音。agent 执行时不超过 5 分钟。

mod common;

use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

use aem_engine::{Engine, EngineConfig, EngineEvent};
use aem_protocol::ids::{NodeId, PlanRevision, SlotDirection};
use aem_protocol::plan::{NodeKind, PlanNode, PlanRoute, Resampler, RoutePlan, Strip};
use aem_protocol::units::{GainDb, SampleRate};
use aem_usbip::identity::IdentityProfile;
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
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output" => options.output = args.next(),
            "--input" => options.input = args.next(),
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
    engine.apply_plan(&plan(endpoint_id(&output_device)?, endpoint_id(&input_device)?)?)?;
    let _attachment = Attachment::attach(PORT, &BUS_IDS);
    let started = Instant::now();

    let seconds = options.seconds;
    let expected = AMPLITUDE * 10f64.powf(f64::from(OUTPUT_GAIN_DB) / DECIBELS_PER_DECADE);
    let window = seconds - TAIL_SECONDS - SETTLE_SECONDS;
    let output_for_loopback = options.output.clone();
    let loopback = thread::spawn(move || -> Result<common::Analysis> {
        wasapi::initialize_mta().ok()?;
        let device = find_device(wasapi::Direction::Render, output_for_loopback.as_deref())?;
        timed(started, capture_sine(&device, seconds + TAIL_SECONDS, window, expected))
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

    while let Some(event) = engine.next_event(Duration::ZERO) {
        match event {
            EngineEvent::Endpoint(event) => println!("endpoint {:?}: {:?}", event.direction, event.status),
            EngineEvent::SlotFormat { slot, format } => println!("slot {slot} format: {format:?}"),
            EngineEvent::SlotConnection { slot, connected } => println!("slot {slot} connected: {connected}"),
        }
    }
    drop(_attachment);
    drop(engine);

    println!("rendered {rendered:.1} s into {RENDER_SLOT}");
    println!(
        "loopback of {output_name} at {} Hz: checked {:.1} s, peak {:.4} (expected {expected:.4}), glitches {}, discontinuity flags {}",
        analysis.sample_rate,
        analysis.checked_seconds(),
        analysis.peak,
        analysis.glitches,
        analysis.discontinuities
    );
    println!("{input_name} → {CAPTURE_SLOT}: peak {input_peak:.5}");
    let passed = (analysis.peak - expected).abs() < expected * 0.1
        && analysis.checked_seconds() >= window * 0.95
        && analysis.glitches == 0;
    println!("{}", if passed { "PASS" } else { "FAIL" });
    Ok(passed)
}

fn plan(output: aem_protocol::ids::EndpointId, input: aem_protocol::ids::EndpointId) -> Result<RoutePlan> {
    let slot_input = NodeId(1);
    let external_output = NodeId(2);
    let external_input = NodeId(3);
    let slot_output = NodeId(4);
    let node = |id, kind| PlanNode {
        id,
        kind,
        strip: Strip::default(),
    };
    Ok(RoutePlan {
        revision: PlanRevision(1),
        engine_sample_rate: SampleRate::new(ENGINE_RATE)?,
        clock_master: Some(external_output),
        resampler: Resampler::default(),
        nodes: vec![
            node(
                slot_input,
                NodeKind::VirtualInput {
                    slot: slot(SlotDirection::In, 1)?,
                },
            ),
            node(external_output, NodeKind::ExternalOutput { endpoint: output }),
            node(external_input, NodeKind::ExternalInput { endpoint: input }),
            node(
                slot_output,
                NodeKind::VirtualOutput {
                    slot: slot(SlotDirection::Out, 1)?,
                },
            ),
        ],
        routes: vec![
            PlanRoute {
                from: slot_input,
                to: external_output,
                gain: GainDb::new(OUTPUT_GAIN_DB)?,
                enabled: true,
            },
            PlanRoute {
                from: external_input,
                to: slot_output,
                gain: GainDb::UNITY,
                enabled: true,
            },
        ],
    })
}
