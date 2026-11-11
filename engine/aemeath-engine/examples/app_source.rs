//! 端到端测试：应用源。
//!
//! 拓扑：应用源（测试播放程序）→ `Aemeath Dev Out 1`。流程：
//! 1. 还没启动程序，应报告"未运行"；
//! 2. 启动程序，应自动接入；从 Dev Out 1 录音，逐样本检查 1 kHz 正弦连续；
//! 3. 结束程序，应报告"未运行"；再启动，应自动接入；
//! 4. 程序文件改名，结束程序后应报告"程序缺失"并停止寻找；改回原名，发一次重新检测，应恢复。
//!
//! 用法：`cargo run -p aemeath-engine --example app_source --release -- <测试播放程序.exe> <播放设备名片段> [秒数]`
//! 测试播放程序要接受两个参数：`<设备名片段> tone:<秒数>`，以幅度 0.05 播放 1 kHz 正弦。agent 执行时不超过 5 分钟。

mod common;

use std::path::PathBuf;
use std::process::{Child, Command, ExitCode};
use std::thread;
use std::time::{Duration, Instant};

use aemeath_engine::{Engine, EngineConfig, EngineEvent};
use aemeath_io::redetect::RedetectTarget;
use aemeath_protocol::ids::{NodeId, ProgramPath, SlotDirection, TopologyRevision};
use aemeath_protocol::message::EndpointStatus;
use aemeath_protocol::topology::{NodeKind, Resampler, RouteTopology, Strip, TopologyNode, TopologyRoute};
use aemeath_protocol::units::{GainDb, SampleRate};
use aemeath_usbip::identity::IdentityProfile;
use common::{Attachment, PORT, Result, capture_sine, find_device, join, slot, timed};

const CAPTURE_SLOT: &str = "Aemeath Dev Out 1";
const BUS_IDS: [&str; 1] = ["1-9"];
const AMPLITUDE: f64 = 0.05;
/// 信号出现后跳过的时间，以及录音末尾留出的余量
const SETTLE_SECONDS: f64 = 1.5;
const TAIL_SECONDS: f64 = 1.0;
/// 状态变化的等待上限：寻找线程 2 s 一轮，再留出余量
const STATUS_TIMEOUT: Duration = Duration::from_secs(8);
/// 启动程序后等多久再开始录音（寻找线程发现进程并开流）
const PLAYER_GRACE_SECONDS: f64 = 4.0;
const SHORT_RUN_SECONDS: u32 = 30;
const SLOT_SETTLE: Duration = Duration::from_secs(8);

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

fn spawn_player(exe: &PathBuf, device: &str, seconds: u32) -> Result<Child> {
    Ok(Command::new(exe)
        .args([device, &format!("tone:{seconds}")])
        .stdout(std::process::Stdio::null())
        .spawn()?)
}

/// 等到引擎报告满足 `wanted` 的状态；超时返回 false。
fn wait_status(engine: &Engine, name: &str, wanted: impl Fn(&EndpointStatus) -> bool) -> bool {
    let deadline = Instant::now() + STATUS_TIMEOUT;
    while Instant::now() < deadline {
        if let Some(EngineEvent::AppSource(event)) = engine.next_event(Duration::from_millis(200)) {
            println!("app source status: {:?}", event.status);
            if wanted(&event.status) {
                println!("OK: {name}");
                return true;
            }
        }
    }
    println!("TIMEOUT waiting for: {name}");
    false
}

fn run() -> Result<bool> {
    tracing_subscriber::fmt().with_max_level(tracing::Level::INFO).init();
    let mut args = std::env::args().skip(1);
    let original = PathBuf::from(args.next().ok_or("missing the test player path")?);
    let device = args.next().ok_or("missing the playback device name")?;
    let seconds: f64 = args.next().map_or(Ok(120.0), |text| text.parse())?;
    wasapi::initialize_mta().ok()?;

    // 用副本测试，改名不影响原文件
    let exe = original.with_file_name("app_source_e2e_player.exe");
    let renamed = original.with_file_name("app_source_e2e_player.moved");
    std::fs::copy(&original, &exe)?;
    let program = ProgramPath::new(exe.to_str().ok_or("the path is not UTF-8")?)?;

    let mut engine = Engine::start(EngineConfig {
        usbip_port: PORT,
        identity: IdentityProfile::Development,
    })?;
    engine.apply_topology(&topology(program.clone())?)?;
    let attachment = Attachment::attach(PORT, &BUS_IDS);
    let mut passed = wait_status(&engine, "not running before the program starts", |status| {
        *status == EndpointStatus::NotRunning
    });

    let player_seconds = seconds + SLOT_SETTLE.as_secs_f64() + PLAYER_GRACE_SECONDS * 2.0;
    let mut player = spawn_player(&exe, &device, player_seconds as u32)?;
    passed &= wait_status(&engine, "connected after the program starts", |status| {
        matches!(status, EndpointStatus::Online { .. })
    });
    // 槽位刚挂上时 Windows 还在配置端点，这时开的流会被作废（AUDCLNT_E_DEVICE_INVALIDATED）
    thread::sleep(SLOT_SETTLE);
    let started = Instant::now();
    let window = seconds - SETTLE_SECONDS - TAIL_SECONDS;
    let capture = thread::spawn(move || -> Result<common::Analysis> {
        wasapi::initialize_mta().ok()?;
        let capture_device = find_device(wasapi::Direction::Capture, Some(CAPTURE_SLOT))?;
        timed(started, capture_sine(&capture_device, seconds, window, AMPLITUDE))
    });
    let analysis = join(&format!("capture from {CAPTURE_SLOT}"), capture)?;
    println!(
        "{CAPTURE_SLOT}: checked {:.1} s, peak {:.4} (expected {AMPLITUDE:.4}), glitches {}, discontinuity flags {}",
        analysis.checked_seconds(),
        analysis.peak,
        analysis.glitches,
        analysis.discontinuities
    );
    passed &= (analysis.peak - AMPLITUDE).abs() < AMPLITUDE * 0.1
        && analysis.checked_seconds() >= window * 0.95
        && analysis.glitches == 0;

    player.kill()?;
    player.wait()?;
    passed &= wait_status(&engine, "not running after the program exits", |status| {
        *status == EndpointStatus::NotRunning
    });

    let mut player = spawn_player(&exe, &device, SHORT_RUN_SECONDS)?;
    passed &= wait_status(&engine, "reconnected when the program starts again", |status| {
        matches!(status, EndpointStatus::Online { .. })
    });
    player.kill()?;
    player.wait()?;
    std::fs::rename(&exe, &renamed)?;
    passed &= wait_status(&engine, "program missing after the file is renamed", |status| {
        *status == EndpointStatus::ProgramMissing
    });

    std::fs::rename(&renamed, &exe)?;
    thread::sleep(Duration::from_secs(4));
    println!("no status change is expected until a redetect");
    let stayed_quiet = engine.next_event(Duration::from_millis(100)).is_none();
    engine.redetect(Some(&[RedetectTarget::Program(program)]));
    passed &= wait_status(&engine, "not running again after a redetect", |status| {
        *status == EndpointStatus::NotRunning
    });
    passed &= stayed_quiet;
    drop(attachment);
    drop(engine);
    let _ = std::fs::remove_file(&exe);
    println!("{}", if passed { "PASS" } else { "FAIL" });
    Ok(passed)
}

fn topology(program: ProgramPath) -> Result<RouteTopology> {
    let source = NodeId(1);
    let output = NodeId(2);
    let node = |id, kind| TopologyNode {
        id,
        kind,
        strip: Strip::default(),
        slot_settings: None,
        io: None,
    };
    Ok(RouteTopology {
        revision: TopologyRevision(1),
        engine_sample_rate: SampleRate::new(48_000)?,
        clock_master: None,
        resampler: Resampler::default(),
        nodes: vec![
            node(
                source,
                NodeKind::AppSource {
                    program,
                    resampler: None,
                },
            ),
            node(
                output,
                NodeKind::VirtualOutput {
                    slot: slot(SlotDirection::Out, 1)?,
                },
            ),
        ],
        routes: vec![TopologyRoute {
            from: source,
            to: output,
            gain: GainDb::UNITY,
            enabled: true,
        }],
    })
}
