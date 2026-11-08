//! 设备拔插自恢复测试：外部设备被拔出、重新连接后，引擎不需要重启就能自动恢复出声。
//!
//! 为了不动真实的设备，用第二个引擎 B 的开发槽位扮演"外部设备"：
//! - 引擎 B（端口 3243）：`Aemeath Dev In 2` → `Aemeath Dev Out 2` 直通。
//! - 引擎 A（端口 3242，被测）：`Dev In 1` → 外部输出 `Dev In 2`；外部输入 `Dev Out 2` → `Dev Out 1`。
//!
//! 测试应用往 Dev In 1 播放正弦波、从 Dev Out 1 录音。中途卸下引擎 B 的两个槽位（相当于拔出），
//! 几秒后重新挂载（相当于重新连接）：引擎 A 的两个外部设备流会收到 `AUDCLNT_E_DEVICE_INVALIDATED`
//! 或停滞，应报告离线，并在设备回来后自动重新打开。
//!
//! 引擎 A 的外部输出以独占方式打开（默认），外部输入以共享方式打开，两条路径都覆盖到。
//!
//! 用法：`cargo run -p aemeath-engine --example recovery --release`（约 80 秒，只使用开发槽位）

mod common;

use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

use aemeath_engine::{Engine, EngineConfig, EngineEvent};
use aemeath_io::IoDirection;
use aemeath_protocol::ids::{EndpointId, NodeId, SlotDirection, TopologyRevision};
use aemeath_protocol::message::EndpointStatus;
use aemeath_protocol::topology::{
    DeviceAccess, DeviceSettings, NodeKind, Resampler, RouteTopology, Strip, TopologyNode, TopologyRoute,
};
use aemeath_protocol::units::{GainDb, SampleRate};
use aemeath_usbip::identity::IdentityProfile;
use common::{
    Attachment, PORT, Result, attach, check_sine, detach_all, endpoint_id, find_device, join, record, render_sine, slot,
};

/// 扮演外部设备的引擎 B 的端口
const DEVICE_PORT: u16 = 3243;
const ENGINE_BUS_IDS: [&str; 2] = ["1-1", "1-9"];
const DEVICE_BUS_IDS: [&str; 2] = ["1-2", "1-10"];
const RENDER_SLOT: &str = "Aemeath Dev In 1";
const CAPTURE_SLOT: &str = "Aemeath Dev Out 1";
const FAKE_OUTPUT: &str = "Aemeath Dev In 2";
const FAKE_INPUT: &str = "Aemeath Dev Out 2";
const AMPLITUDE: f64 = 0.5;
const RECORD_SECONDS: f64 = 75.0;
const UNPLUG_AT: Duration = Duration::from_secs(20);
const UNPLUGGED_FOR: Duration = Duration::from_secs(6);
/// 恢复在线后跳过这段再检查（两级 ASRC 攒水位、控制器回到稳态）
const SETTLE_AFTER_RECOVERY_S: f64 = 3.0;
/// 拔出前检查的时段（从录音开始算起）
const BEFORE_FROM_S: f64 = 5.0;
/// 录音结束前留出的余量
const END_MARGIN_S: f64 = 2.0;
/// 拔出后等待恢复的上限
const RECOVERY_TIMEOUT: Duration = Duration::from_secs(30);
const ENGINE_RATE: u32 = 48_000;
/// 第一跳只录到拔出之前
const FIRST_HOP_SECONDS: f64 = 19.0;
/// 拔出前的检查窗口在拔出时刻之前这么久结束
const UNPLUG_MARGIN_S: f64 = 0.2;

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
    wasapi::initialize_mta().ok()?;

    // 引擎 B：扮演外部设备
    let mut device_engine = Engine::start(EngineConfig {
        usbip_port: DEVICE_PORT,
        identity: IdentityProfile::Development,
    })?;
    device_engine.apply_topology(&passthrough_topology()?)?;
    let device_attachment = Attachment::attach(DEVICE_PORT, &DEVICE_BUS_IDS);
    let fake_output = endpoint_id(&find_device(wasapi::Direction::Render, Some(FAKE_OUTPUT))?)?;
    let fake_input = endpoint_id(&find_device(wasapi::Direction::Capture, Some(FAKE_INPUT))?)?;

    // 引擎 A：被测
    let mut engine = Engine::start(EngineConfig {
        usbip_port: PORT,
        identity: IdentityProfile::Development,
    })?;
    engine.apply_topology(&engine_topology(fake_output.clone(), fake_input.clone())?)?;
    let _engine_attachment = Attachment::attach(PORT, &ENGINE_BUS_IDS);

    let started = Instant::now();
    let recording = thread::spawn(move || -> Result<common::Recording> {
        wasapi::initialize_mta().ok()?;
        record(
            &find_device(wasapi::Direction::Capture, Some(CAPTURE_SLOT))?,
            RECORD_SECONDS,
        )
    });
    // 第一跳之后（引擎 A 的外部输出 → 引擎 B → Dev Out 2）同时录音，用来区分跳变出在哪一段；拔出之前结束
    let first_hop = thread::spawn(move || -> Result<common::Recording> {
        wasapi::initialize_mta().ok()?;
        record(
            &find_device(wasapi::Direction::Capture, Some(FAKE_INPUT))?,
            FIRST_HOP_SECONDS,
        )
    });
    thread::sleep(Duration::from_millis(500));
    let render = thread::spawn(move || render_sine(RENDER_SLOT, RECORD_SECONDS - 1.0, AMPLITUDE));

    // 第 20 秒拔出，6 秒后重新连接
    let mut states = DeviceStates::default();
    wait_until(started, UNPLUG_AT, &engine, &mut states);
    println!(
        "[{:5.1} s] unplugging {FAKE_OUTPUT} and {FAKE_INPUT}",
        started.elapsed().as_secs_f64()
    );
    detach_all(DEVICE_PORT);
    let unplugged_at = started.elapsed().as_secs_f64();
    states.clear();
    wait_until(started, UNPLUG_AT + UNPLUGGED_FOR, &engine, &mut states);
    println!("[{:5.1} s] reconnecting", started.elapsed().as_secs_f64());
    for busid in DEVICE_BUS_IDS {
        attach(DEVICE_PORT, busid);
    }
    // 等引擎 A 报告两个外部设备都先离线、再重新在线
    let deadline = UNPLUG_AT + UNPLUGGED_FOR + RECOVERY_TIMEOUT;
    while states.recovered_at.is_none() && started.elapsed() < deadline {
        observe(started, &engine, &mut states);
    }
    let offline = states.offline;
    let recovered = states.recovered_at;

    let _ = join("rendering", render)?;
    let recording = join("recording", recording)?;
    let first_hop = join("recording the first hop", first_hop)?;
    drop(device_attachment);
    drop(engine);
    drop(device_engine);

    // 设置 AEM_DUMP=<路径> 时，把 Out 1 的录音（左声道，f32 小端）导出，供离线分析
    if let Ok(path) = std::env::var("AEM_DUMP") {
        let bytes: Vec<u8> = recording.left.iter().flat_map(|sample| sample.to_le_bytes()).collect();
        std::fs::write(&path, bytes)?;
        println!("recording written to {path} ({} Hz, mono f32)", recording.sample_rate);
    }
    if !recording.discontinuities.is_empty() {
        println!(
            "recording client reported discontinuities at {:?} s",
            recording.discontinuities
        );
    }
    let hop = check_sine(&first_hop, BEFORE_FROM_S, FIRST_HOP_SECONDS - 0.1, AMPLITUDE);
    println!(
        "first hop ({FAKE_INPUT}) before unplugging: checked {:.1} s, peak {:.3}, glitches {}",
        hop.checked_seconds(),
        hop.peak,
        hop.glitches
    );
    // 测试的时间从挂载完成算起，录音从设备初始化完成才开始；检查窗口要换算到录音自己的时间轴
    let unplugged_in_recording = recording.seconds_since(started, unplugged_at) - UNPLUG_MARGIN_S;
    let before = check_sine(&recording, BEFORE_FROM_S, unplugged_in_recording, AMPLITUDE);
    println!(
        "before unplugging: checked {:.1} s, peak {:.3}, glitches {}",
        before.checked_seconds(),
        before.peak,
        before.glitches
    );
    let Some(recovered) = recovered else {
        println!("the engine did not report both devices online again within {RECOVERY_TIMEOUT:?}");
        println!("FAIL");
        return Ok(false);
    };
    let after_from = recording.seconds_since(started, recovered) + SETTLE_AFTER_RECOVERY_S;
    let after = check_sine(&recording, after_from, RECORD_SECONDS - END_MARGIN_S, AMPLITUDE);
    println!(
        "recovered {:.1} s after reconnecting; from {after_from:.1} s: checked {:.1} s, peak {:.3}, glitches {}",
        recovered - (UNPLUG_AT + UNPLUGGED_FOR).as_secs_f64(),
        after.checked_seconds(),
        after.peak,
        after.glitches
    );
    let level_ok = |peak: f64| (peak - AMPLITUDE).abs() < 0.05;
    let passed = offline == (true, true)
        && before.glitches == 0
        && level_ok(before.peak)
        && after.glitches == 0
        && level_ok(after.peak)
        && after.checked_seconds() > 10.0;
    println!("{}", if passed { "PASS" } else { "FAIL" });
    Ok(passed)
}

/// 引擎 A 两个外部设备（输出、输入）的状态。
#[derive(Default)]
struct DeviceStates {
    offline: (bool, bool),
    online: (bool, bool),
    recovered_at: Option<f64>,
}

impl DeviceStates {
    fn clear(&mut self) {
        *self = Self::default();
    }
}

/// 处理一个引擎 A 的设备事件（最多等 200 ms）：打印，并记录"先离线、再在线"。
fn observe(started: Instant, engine: &Engine, states: &mut DeviceStates) {
    let Some(EngineEvent::Endpoint(event)) = engine.next_event(Duration::from_millis(200)) else {
        return;
    };
    let at = started.elapsed().as_secs_f64();
    println!(
        "[{at:5.1} s] {:?} {}: {:?}",
        event.direction,
        short(&event.endpoint),
        event.status
    );
    let is_online = matches!(event.status, EndpointStatus::Online { .. });
    let (offline, online) = match event.direction {
        IoDirection::Output => (&mut states.offline.0, &mut states.online.0),
        IoDirection::Input => (&mut states.offline.1, &mut states.online.1),
    };
    if !is_online {
        *offline = true;
    }
    *online = is_online && *offline;
    if states.online == (true, true) && states.recovered_at.is_none() {
        states.recovered_at = Some(at);
    }
}

/// 等到 `started` 之后的 `at`，期间处理设备事件。
fn wait_until(started: Instant, at: Duration, engine: &Engine, states: &mut DeviceStates) {
    while started.elapsed() < at {
        observe(started, engine, states);
    }
}

fn short(endpoint: &EndpointId) -> &str {
    endpoint.0.rsplit('.').next().unwrap_or(&endpoint.0)
}

fn node(id: NodeId, kind: NodeKind) -> TopologyNode {
    TopologyNode {
        id,
        kind,
        strip: Strip::default(),
        io: None,
    }
}

fn route(from: NodeId, to: NodeId) -> TopologyRoute {
    TopologyRoute {
        from,
        to,
        gain: GainDb::UNITY,
        enabled: true,
    }
}

fn passthrough_topology() -> Result<RouteTopology> {
    let input = NodeId(1);
    let output = NodeId(2);
    Ok(RouteTopology {
        revision: TopologyRevision(1),
        engine_sample_rate: SampleRate::new(ENGINE_RATE)?,
        clock_master: None,
        resampler: Resampler::default(),
        nodes: vec![
            node(
                input,
                NodeKind::VirtualInput {
                    slot: slot(SlotDirection::In, 2)?,
                },
            ),
            node(
                output,
                NodeKind::VirtualOutput {
                    slot: slot(SlotDirection::Out, 2)?,
                },
            ),
        ],
        routes: vec![route(input, output)],
    })
}

fn engine_topology(fake_output: EndpointId, fake_input: EndpointId) -> Result<RouteTopology> {
    let slot_input = NodeId(1);
    let external_output = NodeId(2);
    let external_input = NodeId(3);
    let slot_output = NodeId(4);
    Ok(RouteTopology {
        revision: TopologyRevision(1),
        engine_sample_rate: SampleRate::new(ENGINE_RATE)?,
        clock_master: None,
        resampler: Resampler::default(),
        nodes: vec![
            node(
                slot_input,
                NodeKind::VirtualInput {
                    slot: slot(SlotDirection::In, 1)?,
                },
            ),
            // 外部输出按默认值独占打开（ADR-0014）；外部输入改为共享，
            // 因为测试还要以共享方式直接录这台"外部设备"（第一跳），独占时它打不开
            node(external_output, NodeKind::ExternalOutput { device: fake_output }),
            TopologyNode {
                io: Some(DeviceSettings {
                    access: DeviceAccess::Shared,
                    resampler: None,
                }),
                ..node(external_input, NodeKind::ExternalInput { device: fake_input })
            },
            node(
                slot_output,
                NodeKind::VirtualOutput {
                    slot: slot(SlotDirection::Out, 1)?,
                },
            ),
        ],
        routes: vec![route(slot_input, external_output), route(external_input, slot_output)],
    })
}
