//! 挂载维护的端到端验证（真机，开发身份，约 1–2 分钟）。拉起一个 `aemeath-host` 子进程，依次验证：
//!
//! 1. 注册：16 个槽位全部挂上、端点 ACTIVE、设备格式的采样率为引擎采样率；
//! 2. 按占用拔出：只占用 In 1，其余 15 个被卸下，In 1 不受影响；
//! 2. 格式：节点指定的格式生效，Windows 里改的格式被改回来，释放后再被占用的槽位不留上一个节点的格式；
//! 3. 引擎崩溃恢复：结束引擎进程再重新拉起，挂载维护把 In 1 挂回来（记录恢复用时）；
//! 4. 清理：全部卸下，请引擎退出。
//!
//! ```text
//! cargo build --release -p aemeath-host
//! cargo run -p aemeath-control --example slot-maintenance -- <aemeath-host.exe> <数据目录>
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use aemeath_control::engine_client::{ClientConfig, ClientEvent, EngineClient, Timing};
use aemeath_control::slots::registration::{RegistrationProgress, all_slots, registration_topology};
use aemeath_control::slots::{MaintainerConfig, MaintainerTiming, SlotHealth, SlotMaintainer, SlotStatus};
use aemeath_control::usbip_cli::UsbipCli;
use aemeath_protocol::ids::{NodeId, SlotDirection, SlotId, SlotNumber, TopologyRevision};
use aemeath_protocol::message::{DeviceFormat, SampleFormat};
use aemeath_protocol::topology::{NodeKind, Resampler, RouteTopology, SlotFormatChoice, Strip, TopologyNode};
use aemeath_protocol::units::SampleRate;
use aemeath_sys::com::ComApartment;
use aemeath_sys::policy_config::PolicyConfig;
use aemeath_usbip::identity::{DEFAULT_USBIP_PORT, IdentityProfile};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

type Error = Box<dyn std::error::Error>;

const REGISTRATION_TIMEOUT: Duration = Duration::from_secs(150);
const STEP_TIMEOUT: Duration = Duration::from_secs(30);
const WINDOWS_POLL: Duration = Duration::from_millis(500);
const RATE: SampleRate = SampleRate::HZ_48000;

struct Harness {
    client: EngineClient,
    maintainer: SlotMaintainer,
    client_events: mpsc::UnboundedReceiver<ClientEvent>,
    statuses: mpsc::UnboundedReceiver<SlotStatus>,
    latest: BTreeMap<SlotId, SlotStatus>,
    progress: RegistrationProgress,
    started: Instant,
}

impl Harness {
    /// 处理事件，直到 `done` 成立或超时。返回是否成立。
    async fn pump_until(&mut self, timeout: Duration, done: impl Fn(&Self) -> bool) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        while !done(self) {
            tokio::select! {
                event = self.client_events.recv() => {
                    let Some(event) = event else { return false };
                    self.maintainer.engine_event(&event);
                    match &event {
                        ClientEvent::SlotFormat { .. } | ClientEvent::EndpointStatus { .. } => {}
                        other => self.log(&format!("engine: {other:?}")),
                    }
                }
                status = self.statuses.recv() => {
                    let Some(status) = status else { return false };
                    let format = status
                        .endpoint
                        .as_ref()
                        .and_then(|endpoint| endpoint.format)
                        .map(|format| format!(" {} Hz {:?}", format.sample_rate.hz(), format.sample_format))
                        .unwrap_or_default();
                    self.log(&format!("slot {}: {:?}{format}", status.slot, status.health));
                    self.progress.update(status.clone());
                    self.latest.insert(status.slot, status);
                }
                () = tokio::time::sleep_until(deadline) => return false,
            }
        }
        true
    }

    fn endpoint_id(&self, slot: SlotId) -> Option<String> {
        Some(self.latest.get(&slot)?.endpoint.as_ref()?.id.clone())
    }

    /// 处理事件的同时轮询 Windows 里的实际格式，直到等于 `wanted` 或超时。
    async fn wait_for_windows_format(&mut self, id: String, wanted: (u32, SampleFormat)) -> bool {
        let deadline = Instant::now() + STEP_TIMEOUT;
        while Instant::now() < deadline {
            self.pump_until(WINDOWS_POLL, |_| false).await;
            let query = id.clone();
            if let Ok(Ok(format)) = tokio::task::spawn_blocking(move || format_in_windows(query)).await
                && format == wanted
            {
                return true;
            }
        }
        false
    }

    fn format(&self, slot: SlotId) -> Option<(u32, SampleFormat)> {
        let format = self.latest.get(&slot)?.endpoint.as_ref()?.format?;
        Some((format.sample_rate.hz(), format.sample_format))
    }

    fn health(&self, slot: SlotId) -> Option<SlotHealth> {
        self.latest.get(&slot).map(|status| status.health)
    }

    fn log(&self, line: &str) {
        println!("[{:7.2}s] {line}", self.started.elapsed().as_secs_f64());
    }
}

fn start_host(program: &str, data_dir: &str, usbip_port: u16) -> Result<Child, Error> {
    Ok(Command::new(program)
        .args(["--data-dir", data_dir, "--usbip-port", &usbip_port.to_string()])
        .kill_on_drop(true)
        .spawn()?)
}

fn in_slot(number: u8) -> Result<SlotId, Error> {
    Ok(SlotId {
        direction: SlotDirection::In,
        number: SlotNumber::new(number)?,
    })
}

/// Windows 里实际的格式（不经过挂载维护的缓存）
fn format_in_windows(endpoint_id: String) -> Result<(u32, SampleFormat), String> {
    let _com = ComApartment::multithreaded().map_err(|error| error.to_string())?;
    let policy = PolicyConfig::new().map_err(|error| error.to_string())?;
    let format = policy.device_format(&endpoint_id).map_err(|error| error.to_string())?;
    Ok((format.sample_rate.hz(), format.sample_format))
}

/// 模拟用户在 Windows 声音设置里改格式
fn change_format_in_windows(endpoint_id: String, format: DeviceFormat) -> Result<(), String> {
    let _com = ComApartment::multithreaded().map_err(|error| error.to_string())?;
    let policy = PolicyConfig::new().map_err(|error| error.to_string())?;
    policy
        .set_device_format(&endpoint_id, format)
        .map_err(|error| error.to_string())
}

fn topology_with(slots: &[SlotId]) -> RouteTopology {
    RouteTopology {
        revision: TopologyRevision(0),
        engine_sample_rate: RATE,
        clock_master: None,
        resampler: Resampler::default(),
        nodes: slots
            .iter()
            .zip(1..)
            .map(|(slot, id)| TopologyNode {
                id: NodeId(id),
                kind: NodeKind::VirtualInput { slot: *slot },
                strip: Strip::default(),
                slot_settings: None,
                io: None,
            })
            .collect(),
        routes: Vec::new(),
    }
}

fn empty_topology() -> RouteTopology {
    RouteTopology {
        revision: TopologyRevision(0),
        engine_sample_rate: RATE,
        clock_master: None,
        resampler: Resampler::default(),
        nodes: Vec::new(),
        routes: Vec::new(),
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("aemeath_control=info")),
        )
        .init();
    let mut args = std::env::args().skip(1);
    let host_program = args
        .next()
        .ok_or("usage: slot-maintenance <aemeath-host.exe> <data dir> [usbip port]")?;
    let data_dir = args
        .next()
        .ok_or("usage: slot-maintenance <aemeath-host.exe> <data dir> [usbip port]")?;
    let usbip_port: u16 = args.next().map_or(Ok(DEFAULT_USBIP_PORT), |text| text.parse())?;
    let identity = IdentityProfile::Development;
    let identities = identity.all()?;
    let usbip = UsbipCli::new(
        UsbipCli::default_program().ok_or("%ProgramFiles% is not set")?,
        usbip_port,
    );

    let mut host = start_host(&host_program, &data_dir, usbip_port)?;
    let (client_tx, client_events) = mpsc::unbounded_channel();
    let client = EngineClient::start(
        ClientConfig {
            pipe_name: identity.engine_pipe_name()?,
            app_version: "slot-maintenance".to_owned(),
            timing: Timing::default(),
        },
        client_tx,
    );
    let (status_tx, statuses) = mpsc::unbounded_channel();
    let maintainer = SlotMaintainer::start(
        MaintainerConfig {
            identity,
            cli: usbip.clone(),
            engine_rate: RATE,
            timing: MaintainerTiming::default(),
        },
        status_tx,
    )?;
    let mut harness = Harness {
        client,
        maintainer,
        client_events,
        statuses,
        latest: BTreeMap::new(),
        progress: RegistrationProgress::new(RATE),
        started: Instant::now(),
    };
    let mut results = Vec::new();

    // 1. 注册
    harness.log("== 1. registration: occupy all 16 slots");
    harness.client.apply(registration_topology(RATE));
    harness.maintainer.set_occupied(all_slots());
    let registered = harness
        .pump_until(REGISTRATION_TIMEOUT, |harness| harness.progress.is_done())
        .await;
    let registration_time = harness.started.elapsed();
    harness.log(&format!(
        "registration: {}/{} slots, failing {:?}",
        harness.progress.completed(),
        harness.progress.total(),
        harness.progress.failing()
    ));
    results.push(("registration", registered, registration_time));

    // 2. 格式由节点决定
    let in_one = in_slot(SlotNumber::FIRST)?;
    let in_two = in_slot(SlotNumber::FIRST + 1)?;
    const FOLLOWING: (u32, SampleFormat) = (48_000, SampleFormat::Int32);
    const FIXED: (u32, SampleFormat) = (44_100, SampleFormat::Int16);
    harness.log("== 2a. a node asks In 2 for 44.1 kHz Int16");
    let step = Instant::now();
    harness.maintainer.set_formats(BTreeMap::from([(
        in_two,
        SlotFormatChoice::Fixed {
            sample_rate: SampleRate::new(FIXED.0)?,
            sample_format: FIXED.1,
        },
    )]));
    let applied = harness
        .pump_until(STEP_TIMEOUT, |harness| harness.format(in_two) == Some(FIXED))
        .await;
    results.push(("fixed format applied", applied, step.elapsed()));

    harness.log("== 2b. change In 1 in Windows, it must come back");
    let step = Instant::now();
    let in_one_id = harness.endpoint_id(in_one).ok_or("In 1 has no endpoint")?;
    let tampered = in_one_id.clone();
    tokio::task::spawn_blocking(move || {
        change_format_in_windows(
            tampered,
            DeviceFormat {
                sample_rate: SampleRate::new(96_000).map_err(|error| error.to_string())?,
                sample_format: SampleFormat::Int16,
                channels: 2,
            },
        )
    })
    .await??;
    let reverted = harness.wait_for_windows_format(in_one_id, FOLLOWING).await;
    harness.log(&format!("reverted in Windows: {reverted}"));
    results.push(("windows change reverted", reverted, step.elapsed()));

    // 只占用 In 1：In 2 的节点没了，它的格式要被重置
    harness.log("== 2c. occupy In 1 only, In 2 is released");
    let in_two_id = harness.endpoint_id(in_two).ok_or("In 2 has no endpoint")?;
    let step = Instant::now();
    harness.maintainer.set_formats(BTreeMap::new());
    harness.client.apply(topology_with(&[in_one]));
    harness.maintainer.set_occupied(BTreeSet::from([in_one]));
    let others: Vec<SlotId> = all_slots().into_iter().filter(|slot| *slot != in_one).collect();
    let unplugged = harness
        .pump_until(STEP_TIMEOUT, |harness| {
            harness.health(in_one) == Some(SlotHealth::Healthy)
                && others
                    .iter()
                    .all(|slot| harness.health(*slot) == Some(SlotHealth::Unplugged))
        })
        .await;
    let aemeath_entries: Vec<String> = usbip
        .imported()
        .await?
        .into_iter()
        .filter(|device| device.belongs_to(&identities))
        .map(|device| device.busid)
        .collect();
    harness.log(&format!("imported Aemeath devices: {aemeath_entries:?}"));
    results.push((
        "unplug unoccupied",
        unplugged && aemeath_entries == ["1-1"],
        step.elapsed(),
    ));

    // 新节点跟随引擎，占用刚释放的 In 2：上一个节点的 44.1 kHz Int16 不能留下
    harness.log("== 2d. a following node takes In 2 again");
    let step = Instant::now();
    harness.client.apply(topology_with(&[in_one, in_two]));
    harness.maintainer.set_occupied(BTreeSet::from([in_one, in_two]));
    let plugged = harness
        .pump_until(STEP_TIMEOUT, |harness| {
            harness.health(in_two) == Some(SlotHealth::Healthy)
        })
        .await;
    let reset = harness.wait_for_windows_format(in_two_id, FOLLOWING).await;
    harness.log(&format!("In 2 follows the engine again in Windows: {reset}"));
    results.push(("next occupant has no leftover format", plugged && reset, step.elapsed()));
    harness.client.apply(topology_with(&[in_one]));
    harness.maintainer.set_occupied(BTreeSet::from([in_one]));
    harness
        .pump_until(STEP_TIMEOUT, |harness| {
            harness.health(in_two) == Some(SlotHealth::Unplugged)
        })
        .await;

    // 3. 引擎崩溃恢复
    harness.log("== 3. kill the engine and start it again");
    host.kill().await?;
    let offline = harness
        .pump_until(STEP_TIMEOUT, |harness| {
            harness.health(in_one) == Some(SlotHealth::EngineOffline)
        })
        .await;
    let step = Instant::now();
    host = start_host(&host_program, &data_dir, usbip_port)?;
    let recovered = harness
        .pump_until(STEP_TIMEOUT, |harness| {
            harness.health(in_one) == Some(SlotHealth::Healthy)
        })
        .await;
    results.push(("recover after engine restart", offline && recovered, step.elapsed()));

    // 4. 清理
    harness.log("== 4. unplug everything and stop the engine");
    let step = Instant::now();
    harness.client.apply(empty_topology());
    harness.maintainer.set_occupied(BTreeSet::new());
    let cleared = harness
        .pump_until(STEP_TIMEOUT, |harness| {
            harness.health(in_one) == Some(SlotHealth::Unplugged)
        })
        .await;
    let confirmed = harness.client.shutdown_engine().await;
    let exited = tokio::time::timeout(STEP_TIMEOUT, host.wait()).await.is_ok();
    results.push(("cleanup and shutdown", cleared && confirmed && exited, step.elapsed()));

    println!();
    for (name, passed, elapsed) in &results {
        let verdict = if *passed { "PASS" } else { "FAIL" };
        println!("{verdict}  {name:<30} {:6.2} s", elapsed.as_secs_f64());
    }
    if results.iter().all(|(_, passed, _)| *passed) {
        Ok(())
    } else {
        Err("some steps failed".into())
    }
}
