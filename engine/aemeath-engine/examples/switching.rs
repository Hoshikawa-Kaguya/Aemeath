//! 切换设备设置时不爆音（M2 计划 2.5、E2E 第 2、3 项）。
//!
//! 与 `recovery` 相同，用第二个引擎 B 的开发槽位扮演"外部设备"，不动真实的设备：
//! - 引擎 B（端口 32412）：`Aemeath Dev In 2` → `Aemeath Dev Out 2` 直通。
//! - 引擎 A（端口 32411，被测）：`Dev In 1` → 外部输出 `Dev In 2`；外部输入 `Dev Out 2`（共享）→ `Dev Out 1`。
//!
//! 测试应用往 Dev In 1 播放 1 kHz 正弦，从 Dev Out 1 录音。运行中依次切换外部输出的设置：
//! 独占（默认）→ 共享 → 共享 + 多项式重采样 → 独占 + 5 ms 缓冲。每次切换设备流都会重建。
//!
//! 判定：
//! - 不爆音：整段录音相邻样本的最大跳变不超过正弦本身的最大斜率的 [`CLICK_FACTOR`] 倍
//!   （淡出淡入是平滑的；没有淡入淡出时，声音以满幅度突然出现或消失，跳变接近幅度本身）；
//! - 两次切换之间（避开切换点前后各 [`SWITCH_MARGIN_S`]）是连续的正弦，没有毛刺。
//!
//! 用法：`cargo run -p aemeath-engine --example switching --release`（约 50 秒，只使用开发槽位）

mod common;

use std::f64::consts::TAU;
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

use aemeath_engine::{Engine, EngineConfig, EngineEvent};
use aemeath_protocol::ids::{EndpointId, NodeId, SlotDirection, TopologyRevision};
use aemeath_protocol::topology::{
    BufferChoice, DeviceAccess, DeviceSettings, FormatChoice, NodeKind, PolyPreset, Resampler, RouteTopology, Strip,
    TopologyNode, TopologyRoute,
};
use aemeath_protocol::units::{BufferMs, GainDb, SampleRate};
use aemeath_usbip::identity::IdentityProfile;
use common::{
    Attachment, PORT, Result, SINE_HZ, check_sine, endpoint_id, find_device, join, record, render_sine, slot,
};

const DEVICE_PORT: u16 = 32412;
const ENGINE_BUS_IDS: [&str; 2] = ["1-1", "1-9"];
const DEVICE_BUS_IDS: [&str; 2] = ["1-2", "1-10"];
const RENDER_SLOT: &str = "Aemeath Dev In 1";
const CAPTURE_SLOT: &str = "Aemeath Dev Out 1";
const FAKE_OUTPUT: &str = "Aemeath Dev In 2";
const FAKE_INPUT: &str = "Aemeath Dev Out 2";
const AMPLITUDE: f64 = 0.5;
const RECORD_SECONDS: f64 = 48.0;
/// 切换的时刻（从测试开始算起）
const SWITCH_AT_S: [f64; 3] = [14.0, 24.0, 34.0];
/// 第一段检查从这里开始（避开开流的瞬态）
const FIRST_FROM_S: f64 = 5.0;
/// 切换点前后不检查连续性的时长：设备流重建期间会有短暂的空白
const SWITCH_MARGIN_S: f64 = 3.0;
const END_MARGIN_S: f64 = 2.0;
/// 允许的最大跳变 = 正弦最大斜率 × 这个倍数
const CLICK_FACTOR: f64 = 1.5;
const ENGINE_RATE: u32 = 48_000;
const CUSTOM_BUFFER_MS: f32 = 5.0;

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

    let mut device_engine = Engine::start(EngineConfig {
        usbip_port: DEVICE_PORT,
        identity: IdentityProfile::Development,
    })?;
    device_engine.apply_topology(&passthrough_topology()?)?;
    let device_attachment = Attachment::attach(DEVICE_PORT, &DEVICE_BUS_IDS);
    let fake_output = endpoint_id(&find_device(wasapi::Direction::Render, Some(FAKE_OUTPUT))?)?;
    let fake_input = endpoint_id(&find_device(wasapi::Direction::Capture, Some(FAKE_INPUT))?)?;

    let steps = [
        ("exclusive (default)", DeviceSettings::default()),
        (
            "shared",
            DeviceSettings {
                access: DeviceAccess::Shared,
                resampler: None,
            },
        ),
        (
            "shared, polynomial resampler",
            DeviceSettings {
                access: DeviceAccess::Shared,
                resampler: Some(Resampler::PolyAsync {
                    preset: PolyPreset::Fine,
                }),
            },
        ),
        (
            "exclusive, 5 ms buffer",
            DeviceSettings {
                access: DeviceAccess::Exclusive {
                    format: FormatChoice::Recommended,
                    buffer: BufferChoice::Milliseconds {
                        ms: BufferMs::new(CUSTOM_BUFFER_MS)?,
                    },
                },
                resampler: None,
            },
        ),
    ];

    let mut engine = Engine::start(EngineConfig {
        usbip_port: PORT,
        identity: IdentityProfile::Development,
    })?;
    let mut revision = 1;
    engine.apply_topology(&engine_topology(&fake_output, &fake_input, steps[0].1, revision)?)?;
    let _engine_attachment = Attachment::attach(PORT, &ENGINE_BUS_IDS);

    let started = Instant::now();
    let recording = thread::spawn(move || -> Result<common::Recording> {
        wasapi::initialize_mta().ok()?;
        record(
            &find_device(wasapi::Direction::Capture, Some(CAPTURE_SLOT))?,
            RECORD_SECONDS,
        )
    });
    thread::sleep(Duration::from_millis(500));
    let render = thread::spawn(move || render_sine(RENDER_SLOT, RECORD_SECONDS - 1.0, AMPLITUDE));

    let mut switched_at = Vec::new();
    for (at, (name, settings)) in SWITCH_AT_S.iter().zip(steps.iter().skip(1)) {
        while started.elapsed().as_secs_f64() < *at {
            print_events(started, &engine);
        }
        revision += 1;
        let before = Instant::now();
        engine.apply_topology(&engine_topology(&fake_output, &fake_input, *settings, revision)?)?;
        let elapsed = started.elapsed().as_secs_f64();
        println!(
            "[{elapsed:5.1} s] switched the external output to {name} ({} ms)",
            before.elapsed().as_millis()
        );
        switched_at.push(elapsed);
    }
    while started.elapsed().as_secs_f64() < RECORD_SECONDS {
        print_events(started, &engine);
    }

    let _ = join("rendering", render)?;
    let recording = join("recording", recording)?;
    drop(device_attachment);
    drop(engine);
    drop(device_engine);

    // 不爆音：每个切换点前后，以及整段（开头的开流除外）相邻样本的最大跳变
    let rate = f64::from(recording.sample_rate);
    let slope_limit = AMPLITUDE * TAU * SINE_HZ / rate * CLICK_FACTOR;
    let largest_jump = |from_s: f64, to_s: f64| -> (f64, f64) {
        let to = ((to_s * rate) as usize).min(recording.left.len());
        let from = ((from_s * rate) as usize).min(to);
        let (jump, index) = recording.left[from..to]
            .windows(2)
            .enumerate()
            .map(|(index, pair)| ((f64::from(pair[1]) - f64::from(pair[0])).abs(), index + from + 1))
            .fold((0.0, 0), |best, item| if item.0 > best.0 { item } else { best });
        (jump, index as f64 / rate)
    };
    for (switched, (name, _)) in switched_at.iter().zip(steps.iter().skip(1)) {
        let in_recording = recording.seconds_since(started, *switched);
        let (jump, at) = largest_jump(in_recording - SWITCH_MARGIN_S, in_recording + SWITCH_MARGIN_S);
        println!("switch to {name} at {in_recording:.2} s: largest jump {jump:.4} at {at:.3} s");
    }
    let (max_jump, at) = largest_jump(FIRST_FROM_S, RECORD_SECONDS - END_MARGIN_S);
    println!("largest sample-to-sample jump {max_jump:.4} at {at:.3} s (limit {slope_limit:.4})");

    // 两次切换之间是连续的正弦
    let mut bounds = vec![FIRST_FROM_S];
    for switched in &switched_at {
        let in_recording = recording.seconds_since(started, *switched);
        bounds.push(in_recording - SWITCH_MARGIN_S);
        bounds.push(in_recording + SWITCH_MARGIN_S);
    }
    bounds.push(RECORD_SECONDS - END_MARGIN_S);
    let mut segments_ok = true;
    for (index, [from, to]) in bounds.as_chunks::<2>().0.iter().enumerate() {
        let analysis = check_sine(&recording, *from, *to, AMPLITUDE);
        println!(
            "segment {} ({}): {from:.1}–{to:.1} s, checked {:.1} s, peak {:.3}, glitches {}",
            index + 1,
            steps[index].0,
            analysis.checked_seconds(),
            analysis.peak,
            analysis.glitches
        );
        segments_ok &= analysis.glitches == 0 && (analysis.peak - AMPLITUDE).abs() < 0.05;
    }
    let passed = max_jump <= slope_limit && segments_ok;
    println!("{}", if passed { "PASS" } else { "FAIL" });
    Ok(passed)
}

fn print_events(started: Instant, engine: &Engine) {
    if let Some(EngineEvent::Endpoint(event)) = engine.next_event(Duration::from_millis(200)) {
        println!(
            "[{:5.1} s] {:?}: {:?}",
            started.elapsed().as_secs_f64(),
            event.direction,
            event.status
        );
    }
}

fn node(id: NodeId, kind: NodeKind) -> TopologyNode {
    TopologyNode {
        id,
        kind,
        strip: Strip::default(),
        slot_settings: None,
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

fn engine_topology(
    fake_output: &EndpointId,
    fake_input: &EndpointId,
    output_settings: DeviceSettings,
    revision: u64,
) -> Result<RouteTopology> {
    let slot_input = NodeId(1);
    let external_output = NodeId(2);
    let external_input = NodeId(3);
    let slot_output = NodeId(4);
    Ok(RouteTopology {
        revision: TopologyRevision(revision),
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
            TopologyNode {
                slot_settings: None,
                io: Some(output_settings),
                ..node(
                    external_output,
                    NodeKind::ExternalOutput {
                        device: fake_output.clone(),
                    },
                )
            },
            // 外部输入保持共享，不参与切换
            TopologyNode {
                slot_settings: None,
                io: Some(DeviceSettings {
                    access: DeviceAccess::Shared,
                    resampler: None,
                }),
                ..node(
                    external_input,
                    NodeKind::ExternalInput {
                        device: fake_input.clone(),
                    },
                )
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
