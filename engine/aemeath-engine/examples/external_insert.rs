//! 端到端测试：外部插入（M3 计划 E2E 第 3 项）。
//!
//! 拓扑：`Aemeath Dev In 1` → 外部插入（送出端 Dev Out 2，取回端用真实录音设备代替外部程序的输出）→ `Aemeath Dev Out 1`。
//! - 送出端：往 In 1 放 1 kHz 正弦，从 Dev Out 2 录音，逐样本检查连续；
//! - 取回端：确认信号从节点发出到 Dev Out 1（内容无法预知，只看峰值）。
//!
//! 经过 NVIDIA Broadcast 的往返由维护者验收。
//!
//! 用法：`cargo run -p aemeath-engine --example external_insert --release -- <录音设备名片段> [秒数]`。

mod common;

use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

use aemeath_engine::{Engine, EngineConfig, EngineEvent};
use aemeath_protocol::ids::{NodeId, SlotDirection, TopologyRevision};
use aemeath_protocol::message::EndpointStatus;
use aemeath_protocol::topology::{
    DeviceAccess, DeviceSettings, NodeKind, Resampler, RouteTopology, Strip, TopologyNode, TopologyRoute,
};
use aemeath_protocol::units::{GainDb, SampleRate};
use aemeath_usbip::identity::IdentityProfile;
use common::{
    Attachment, PORT, Result, capture_peak, capture_sine, endpoint_id, find_device, join, render_sine, slot, timed,
};

const RENDER_SLOT: &str = "Aemeath Dev In 1";
const RETURN_SLOT: &str = "Aemeath Dev Out 1";
const SEND_SLOT: &str = "Aemeath Dev Out 2";
const BUS_IDS: [&str; 3] = ["1-1", "1-9", "1-10"];
const AMPLITUDE: f64 = 0.5;
const SLOT_SETTLE: Duration = Duration::from_secs(10);
const SETTLE_SECONDS: f64 = 1.0;
const TAIL_SECONDS: f64 = 1.0;

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
    tracing_subscriber::fmt().with_max_level(tracing::Level::WARN).init();
    let mut args = std::env::args().skip(1);
    let device_name = args.next().ok_or("missing the capture device name")?;
    let seconds: f64 = args.next().map_or(Ok(30.0), |text| text.parse())?;
    wasapi::initialize_mta().ok()?;
    let device = find_device(wasapi::Direction::Capture, Some(&device_name))?;

    let (source, insert, output) = (NodeId(1), NodeId(2), NodeId(3));
    let node = |id, kind: NodeKind| TopologyNode {
        id,
        io: kind.is_external().then_some(DeviceSettings {
            access: DeviceAccess::Shared,
            resampler: None,
        }),
        kind,
        strip: Strip::default(),
    };
    let route = |from, to| TopologyRoute {
        from,
        to,
        gain: GainDb::UNITY,
        enabled: true,
    };
    let topology = RouteTopology {
        revision: TopologyRevision(1),
        engine_sample_rate: SampleRate::new(48_000)?,
        clock_master: None,
        resampler: Resampler::default(),
        nodes: vec![
            node(
                source,
                NodeKind::VirtualInput {
                    slot: slot(SlotDirection::In, 1)?,
                },
            ),
            node(
                insert,
                NodeKind::ExternalInsert {
                    send: slot(SlotDirection::Out, 2)?,
                    ret: endpoint_id(&device)?,
                },
            ),
            node(
                output,
                NodeKind::VirtualOutput {
                    slot: slot(SlotDirection::Out, 1)?,
                },
            ),
        ],
        routes: vec![route(source, insert), route(insert, output)],
    };

    let mut engine = Engine::start(EngineConfig {
        usbip_port: PORT,
        identity: IdentityProfile::Development,
    })?;
    engine.apply_topology(&topology)?;
    let attachment = Attachment::attach(PORT, &BUS_IDS);
    thread::sleep(SLOT_SETTLE);
    let mut online = false;
    while let Some(event) = engine.next_event(Duration::ZERO) {
        if let EngineEvent::Endpoint(event) = event {
            println!("return device: {:?}", event.status);
            online |= matches!(event.status, EndpointStatus::Online { .. });
        }
    }

    let started = Instant::now();
    let window = seconds - SETTLE_SECONDS - TAIL_SECONDS;
    let send = thread::spawn(move || -> Result<common::Analysis> {
        wasapi::initialize_mta().ok()?;
        let capture = find_device(wasapi::Direction::Capture, Some(SEND_SLOT))?;
        timed(started, capture_sine(&capture, seconds, window, AMPLITUDE))
    });
    let ret = thread::spawn(move || -> Result<f64> {
        wasapi::initialize_mta().ok()?;
        let capture = find_device(wasapi::Direction::Capture, Some(RETURN_SLOT))?;
        timed(started, capture_peak(&capture, seconds))
    });
    thread::sleep(Duration::from_millis(300));
    let render = thread::spawn(move || timed(started, render_sine(RENDER_SLOT, seconds, AMPLITUDE)));
    join("render", render)?;
    let analysis = join("send capture", send)?;
    let return_peak = join("return capture", ret)?;
    drop(attachment);
    drop(engine);

    println!(
        "send {SEND_SLOT}: checked {:.1} s, peak {:.4}, glitches {}",
        analysis.checked_seconds(),
        analysis.peak,
        analysis.glitches
    );
    println!("return device online: {online}, {RETURN_SLOT} peak: {return_peak:.5}");
    let passed = online
        && analysis.glitches == 0
        && (analysis.peak - AMPLITUDE).abs() < AMPLITUDE * 0.05
        && analysis.checked_seconds() >= window * 0.9;
    println!("{}", if passed { "PASS" } else { "FAIL" });
    Ok(passed)
}
