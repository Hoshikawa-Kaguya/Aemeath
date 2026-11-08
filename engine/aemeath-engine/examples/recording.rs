//! 端到端测试：录音节点（M3 计划 E2E 第 4 项）。
//!
//! 拓扑：`Aemeath Dev In 1` → 录音节点。往 In 1 播放 1 kHz 正弦，WAV 和 FLAC 各录一段，解码后逐样本检查连续性；
//! 再录一段时让子进程在录音中途直接 abort，检查留下的文件仍然可读。
//!
//! 用法：`cargo run -p aemeath-engine --example recording --release -- <输出文件夹> [每段秒数]`。agent 执行时不超过 5 分钟。

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::thread;
use std::time::Duration;

use aemeath_engine::{Engine, EngineConfig};
use aemeath_protocol::ids::{NodeId, SlotDirection, TopologyRevision};
use aemeath_protocol::topology::{
    NodeKind, RecordDepth, RecordFormat, RecorderSettings, Resampler, RouteTopology, Strip, TopologyNode, TopologyRoute,
};
use aemeath_protocol::units::{GainDb, SampleRate};
use aemeath_usbip::identity::IdentityProfile;
use common::{Attachment, PORT, Recording, Result, check_sine, detach_all, render_sine, slot};

const RENDER_SLOT: &str = "Aemeath Dev In 1";
const BUS_IDS: [&str; 1] = ["1-1"];
const AMPLITUDE: f64 = 0.5;
const RECORDER: NodeId = NodeId(2);
const SLOT_SETTLE: Duration = Duration::from_secs(8);
const LEAD_IN: Duration = Duration::from_secs(1);
/// 开头的淡入和结尾的余量不检查
const SKIP_SECONDS: f64 = 0.5;
const CRASH_AFTER: Duration = Duration::from_secs(4);
const FULL_SCALE_24: f64 = 8_388_608.0;

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

fn settings(format: RecordFormat) -> RecorderSettings {
    RecorderSettings {
        format,
        depth: RecordDepth::Int24,
    }
}

fn topology(format: RecordFormat) -> Result<RouteTopology> {
    let source = NodeId(1);
    let node = |id, kind| TopologyNode {
        id,
        kind,
        strip: Strip::default(),
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
                NodeKind::VirtualInput {
                    slot: slot(SlotDirection::In, 1)?,
                },
            ),
            node(
                RECORDER,
                NodeKind::Recorder {
                    settings: settings(format),
                },
            ),
        ],
        routes: vec![TopologyRoute {
            from: source,
            to: RECORDER,
            gain: GainDb::UNITY,
            enabled: true,
        }],
    })
}

/// 启动引擎、挂上 In 1、开始播放，录 `seconds` 秒；`crash` 时在录音中途直接结束进程。
fn record(path: &Path, format: RecordFormat, seconds: f64, crash: bool) -> Result<()> {
    wasapi::initialize_mta().ok()?;
    let mut engine = Engine::start(EngineConfig {
        usbip_port: PORT,
        identity: IdentityProfile::Development,
    })?;
    engine.apply_topology(&topology(format)?)?;
    let attachment = Attachment::attach(PORT, &BUS_IDS);
    thread::sleep(SLOT_SETTLE);
    let render_seconds = seconds + LEAD_IN.as_secs_f64() * 2.0;
    let render = thread::spawn(move || render_sine(RENDER_SLOT, render_seconds, AMPLITUDE));
    thread::sleep(LEAD_IN);
    assert!(engine.start_recording(RECORDER, path.to_path_buf()));
    if crash {
        thread::sleep(CRASH_AFTER);
        std::process::abort();
    }
    thread::sleep(Duration::from_secs_f64(seconds));
    assert!(engine.stop_recording(RECORDER));
    render.join().map_err(|_| "render panicked")??;
    drop(attachment);
    drop(engine);
    Ok(())
}

fn decode(path: &Path) -> Result<Recording> {
    let (sample_rate, left): (u32, Vec<f32>) = if path.extension().is_some_and(|extension| extension == "wav") {
        let mut reader = hound_reader(path)?;
        let rate = reader.spec().sample_rate;
        let left = reader
            .samples::<i32>()
            .step_by(2)
            .map_while(std::result::Result::ok)
            .map(|sample| (f64::from(sample) / FULL_SCALE_24) as f32)
            .collect();
        (rate, left)
    } else {
        let mut reader = claxon::FlacReader::open(path)?;
        let rate = reader.streaminfo().sample_rate;
        let left = reader
            .samples()
            .step_by(2)
            .map_while(std::result::Result::ok)
            .map(|sample| (f64::from(sample) / FULL_SCALE_24) as f32)
            .collect();
        (rate, left)
    };
    Ok(Recording {
        sample_rate,
        left,
        discontinuities: Vec::new(),
        started_at: std::time::Instant::now(),
    })
}

fn hound_reader(path: &Path) -> Result<hound::WavReader<std::io::BufReader<std::fs::File>>> {
    Ok(hound::WavReader::open(path)?)
}

fn verify(path: &Path, label: &str) -> Result<bool> {
    let recording = decode(path)?;
    let length = recording.left.len() as f64 / f64::from(recording.sample_rate);
    let analysis = check_sine(&recording, SKIP_SECONDS, length - SKIP_SECONDS, AMPLITUDE);
    println!(
        "{label}: {length:.1} s, checked {:.1} s, peak {:.4}, glitches {}",
        analysis.checked_seconds(),
        analysis.peak,
        analysis.glitches
    );
    Ok(analysis.glitches == 0 && (analysis.peak - AMPLITUDE).abs() < AMPLITUDE * 0.05)
}

fn run() -> Result<bool> {
    let mut args = std::env::args().skip(1);
    let first = args.next().ok_or("missing the output folder")?;
    if first == "--crash-child" {
        let path = PathBuf::from(args.next().ok_or("missing the path")?);
        record(&path, RecordFormat::Flac, 60.0, true)?;
        return Ok(true);
    }
    // 每段录音一个子进程：同一进程里连续启动引擎时 USB/IP 端口还没有释放
    if first == "--take" {
        let path = PathBuf::from(args.next().ok_or("missing the path")?);
        let format = if path.extension().is_some_and(|extension| extension == "wav") {
            RecordFormat::Wav
        } else {
            RecordFormat::Flac
        };
        let seconds: f64 = args.next().ok_or("missing the seconds")?.parse()?;
        record(&path, format, seconds, false)?;
        return Ok(true);
    }
    tracing_subscriber::fmt().with_max_level(tracing::Level::WARN).init();
    let folder = PathBuf::from(first);
    let seconds: f64 = args.next().map_or(Ok(60.0), |text| text.parse())?;
    std::fs::create_dir_all(&folder)?;

    let mut passed = true;
    for name in ["take.wav", "take.flac"] {
        let path = folder.join(name);
        let _ = std::fs::remove_file(&path);
        let status = Command::new(std::env::current_exe()?)
            .args(["--take", &path.display().to_string(), &seconds.to_string()])
            .status()?;
        passed &= status.success() && verify(&path, name)?;
    }

    let crashed = folder.join("crash.flac");
    let _ = std::fs::remove_file(&crashed);
    let status = Command::new(std::env::current_exe()?)
        .args(["--crash-child", &crashed.display().to_string()])
        .status()?;
    println!("child exit: {status}");
    detach_all(PORT);
    let survived = decode(&crashed)?;
    let length = survived.left.len() as f64 / f64::from(survived.sample_rate);
    println!("crash.flac: {length:.1} s readable after abort");
    passed &= length > 1.0 && verify(&crashed, "crash.flac")?;
    println!("{}", if passed { "PASS" } else { "FAIL" });
    Ok(passed)
}
