//! 挂载维护（ADR-0007）：持续检查每个被占用的槽位是否正常挂载，发现异常就修复；没有被占用的槽位保持卸下。
//!
//! - 何时检查：每 [`MaintainerTiming::check_interval`] 一次（也覆盖了睡眠唤醒）；占用状态改变、引擎连上、
//!   端点有变化、服务端报告某个占用中的槽位断开时立即检查。
//! - 修复：只停掉这台设备自己的后台重试（`attach --stop`，不能用 `--stop-all`），卸下指向 Aemeath 的残留和重复条目，
//!   再以 `--once` 挂载；以服务端实际收到 import 为成功判据。失败后按 1、2、4、8、10 秒退避，
//!   连续 [`ALERT_AFTER_FAILURES`] 次失败标记为 [`SlotHealth::Failing`]，由界面提示用户。
//! - 格式：端点首次变为 ACTIVE 时，设备格式的采样率与引擎不同就改成引擎采样率（样本格式不变）。
//!   槽位的固定比率重采样在 M4 加入，在那之前采样率不同的槽位是静音的。
//! - 只碰指向 Aemeath 服务端口的条目，其他软件（DS4Windows、VIIPER 等）的条目一律不碰。

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use aemeath_protocol::ids::SlotId;
use aemeath_protocol::units::SampleRate;
use aemeath_sys::audio_endpoints::EndpointState;
use aemeath_usbip::identity::{IdentityProfile, SlotIdentity};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, info, warn};

use super::assess::{Assessment, Observation, RepairReason, assess};
use super::endpoints::{EndpointProbe, ProbeError, SlotEndpoint};
use crate::engine_client::ClientEvent;
use crate::usbip_cli::{ImportedDevice, UsbipCli, VhciPort};

/// 失败后的重试间隔（ADR-0007：1、2、4 秒起，上限 10 秒）
const RETRY_DELAYS: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(10),
];
/// 连续失败这么多次后提示用户
pub const ALERT_AFTER_FAILURES: u32 = 3;

#[derive(Debug, Clone, Copy)]
pub struct MaintainerTiming {
    pub check_interval: Duration,
    /// 连接建立后等待端点变为 ACTIVE 的时间；首次安装驱动可能接近 1 分钟（ADR-0009）
    pub endpoint_grace: Duration,
    /// 挂载命令成功后，等待服务端报告收到 import 的时间
    pub attach_confirm: Duration,
    /// 有槽位在等待端点出现时，额外检查的间隔
    pub settle_poll: Duration,
    /// 查询端点的超时：超时就把依赖端点的判定留到下一轮
    pub endpoint_query_timeout: Duration,
    /// 引擎连上后先等这么久再检查：让引擎补发的槽位连接快照先到，
    /// 否则主程序重启时会把正常挂着的槽位误判为"服务端没有连接"而重新挂载
    pub after_engine_up: Duration,
    /// 两次检查之间的最短间隔（端点通知可能密集到达）
    pub min_spacing: Duration,
}

impl Default for MaintainerTiming {
    fn default() -> Self {
        Self {
            check_interval: Duration::from_secs(5),
            endpoint_grace: Duration::from_secs(90),
            attach_confirm: Duration::from_secs(3),
            settle_poll: Duration::from_secs(1),
            endpoint_query_timeout: Duration::from_secs(2),
            after_engine_up: Duration::from_millis(500),
            min_spacing: Duration::from_millis(300),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MaintainerConfig {
    pub identity: IdentityProfile,
    pub cli: UsbipCli,
    pub engine_rate: SampleRate,
    pub timing: MaintainerTiming,
}

/// 槽位的挂载健康状态（设备页显示，ADR-0009）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotHealth {
    /// 没有被占用，已卸下
    Unplugged,
    /// 被占用，但引擎没有运行（守护进程会拉起它）
    EngineOffline,
    /// 已连接，等待 Windows 让端点出现
    Settling,
    Healthy,
    /// 修复失败，稍后重试
    Retrying {
        failures: u32,
        reason: RepairReason,
    },
    /// 多次修复失败，需要提示用户
    Failing {
        failures: u32,
        reason: RepairReason,
    },
}

/// 槽位状态的变化。只在健康状态或端点信息改变时发出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotStatus {
    pub slot: SlotId,
    pub health: SlotHealth,
    pub endpoint: Option<SlotEndpoint>,
}

enum Input {
    Occupied(BTreeSet<SlotId>),
    EngineRate(SampleRate),
    EngineUp,
    EngineDown,
    SlotConnection { slot: SlotId, connected: bool },
    EndpointsChanged,
    CheckNow,
}

/// 挂载维护的句柄。丢弃时后台任务随之结束（不卸下任何设备）。
#[derive(Debug)]
pub struct SlotMaintainer {
    inputs: mpsc::UnboundedSender<Input>,
    task: tokio::task::JoinHandle<()>,
}

impl SlotMaintainer {
    /// 在当前 tokio 运行时上启动。初始时没有槽位被占用、引擎视为未连接。
    pub fn start(config: MaintainerConfig, statuses: mpsc::UnboundedSender<SlotStatus>) -> Result<Self, ProbeError> {
        let identities = config
            .identity
            .all()
            .map_err(|error| ProbeError::new(error.to_string()))?;
        let (inputs, receiver) = mpsc::unbounded_channel();
        let notify = inputs.clone();
        let probe = EndpointProbe::start(identities.clone(), move || {
            let _ = notify.send(Input::EndpointsChanged);
        })?;
        let slots = identities
            .iter()
            .map(|identity| (identity.slot, SlotState::default()))
            .collect();
        let task = Task {
            inputs: receiver,
            cli: config.cli,
            probe,
            statuses,
            identities,
            timing: config.timing,
            occupied: BTreeSet::new(),
            engine_up: false,
            rate: config.engine_rate,
            slots,
            check_requested: false,
            check_not_before: Instant::now(),
            known_endpoints: BTreeMap::new(),
            pending_refresh: Vec::new(),
        };
        Ok(Self {
            inputs,
            task: tokio::spawn(task.run()),
        })
    }

    /// 当前被占用的槽位（来自当前拓扑）。
    pub fn set_occupied(&self, slots: BTreeSet<SlotId>) {
        let _ = self.inputs.send(Input::Occupied(slots));
    }

    pub fn set_engine_rate(&self, rate: SampleRate) {
        let _ = self.inputs.send(Input::EngineRate(rate));
    }

    /// 转交 IPC 客户端的事件（只用到连接状态和槽位连接）。
    pub fn engine_event(&self, event: &ClientEvent) {
        let input = match event {
            ClientEvent::Connected { .. } => Input::EngineUp,
            ClientEvent::Disconnected(_) => Input::EngineDown,
            ClientEvent::SlotConnection { slot, connected } => Input::SlotConnection {
                slot: *slot,
                connected: *connected,
            },
            _ => return,
        };
        let _ = self.inputs.send(input);
    }

    /// 立即检查一轮（例如系统从睡眠唤醒后）。
    pub fn check_now(&self) {
        let _ = self.inputs.send(Input::CheckNow);
    }
}

impl Drop for SlotMaintainer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Debug, Default)]
struct SlotState {
    /// 服务端报告连接建立的时刻
    connected_since: Option<Instant>,
    failures: u32,
    last_failure: Option<RepairReason>,
    retry_at: Option<Instant>,
    /// 这次连接是否已经核对过格式
    format_checked: bool,
    reported: Option<(SlotHealth, Option<SlotEndpoint>)>,
}

struct Task {
    inputs: mpsc::UnboundedReceiver<Input>,
    cli: UsbipCli,
    probe: EndpointProbe,
    statuses: mpsc::UnboundedSender<SlotStatus>,
    identities: Vec<SlotIdentity>,
    timing: MaintainerTiming,
    occupied: BTreeSet<SlotId>,
    engine_up: bool,
    rate: SampleRate,
    slots: BTreeMap<SlotId, SlotState>,
    check_requested: bool,
    check_not_before: Instant,
    /// 最近一次查询到的端点（查询超时时沿用）
    known_endpoints: BTreeMap<SlotId, SlotEndpoint>,
    /// 端点信息有变化、需要重新报告的槽位（避免在遍历 slots 时修改它）
    pending_refresh: Vec<(SlotId, SlotHealth, Option<SlotEndpoint>)>,
}

impl Task {
    async fn run(mut self) {
        let mut next_periodic = Instant::now() + self.timing.check_interval;
        loop {
            let wake = self.next_wake(next_periodic);
            tokio::select! {
                input = self.inputs.recv() => {
                    let Some(input) = input else {
                        return;
                    };
                    self.apply(input);
                }
                () = tokio::time::sleep_until(wake) => self.check_requested = true,
            }
            while let Ok(input) = self.inputs.try_recv() {
                self.apply(input);
            }
            if !self.check_requested {
                continue;
            }
            if !self.engine_up {
                // 引擎连上时会再请求检查
                self.check_requested = false;
                continue;
            }
            let now = Instant::now();
            if now < self.check_not_before {
                continue;
            }
            self.check_requested = false;
            self.check().await;
            next_periodic = Instant::now() + self.timing.check_interval;
            self.check_not_before = Instant::now() + self.timing.min_spacing;
        }
    }

    /// 下一次需要醒来的时刻：定时检查、到期的重试、等待端点出现的轮询、被推迟的检查。
    fn next_wake(&self, next_periodic: Instant) -> Instant {
        let mut wake = next_periodic;
        if self.check_requested {
            wake = wake.min(self.check_not_before);
        }
        for state in self.slots.values() {
            if let Some(retry_at) = state.retry_at {
                wake = wake.min(retry_at);
            }
            if matches!(state.reported, Some((SlotHealth::Settling, _))) {
                wake = wake.min(Instant::now() + self.timing.settle_poll);
            }
        }
        wake
    }

    fn apply(&mut self, input: Input) {
        let now = Instant::now();
        match input {
            Input::Occupied(slots) => {
                if slots != self.occupied {
                    info!(occupied = ?slots, "occupied slots changed");
                    self.occupied = slots;
                    self.check_requested = true;
                }
            }
            Input::EngineRate(rate) => {
                if rate != self.rate {
                    self.rate = rate;
                    for state in self.slots.values_mut() {
                        state.format_checked = false;
                    }
                    self.check_requested = true;
                }
            }
            Input::EngineUp => {
                self.engine_up = true;
                self.check_requested = true;
                self.check_not_before = now + self.timing.after_engine_up;
            }
            Input::EngineDown => {
                self.engine_up = false;
                for state in self.slots.values_mut() {
                    state.connected_since = None;
                    state.format_checked = false;
                }
                for slot in self.occupied.clone() {
                    self.report(slot, SlotHealth::EngineOffline, None);
                }
            }
            Input::SlotConnection { slot, connected } => {
                let Some(state) = self.slots.get_mut(&slot) else {
                    return;
                };
                if connected {
                    state.connected_since.get_or_insert(now);
                } else {
                    state.connected_since = None;
                    state.format_checked = false;
                    if self.occupied.contains(&slot) {
                        info!(%slot, "occupied slot lost its connection");
                        self.check_requested = true;
                    }
                }
            }
            Input::EndpointsChanged | Input::CheckNow => self.check_requested = true,
        }
    }

    async fn check(&mut self) {
        let imported: Vec<ImportedDevice> = match self.cli.imported().await {
            Ok(devices) => devices
                .into_iter()
                .filter(|device| device.is_served_by(self.cli.server_port()))
                .collect(),
            Err(error) => {
                warn!(%error, "cannot list imported devices");
                return;
            }
        };
        // 指向 Aemeath 端口、但 busid 不属于任何槽位的条目（例如旧版本的身份）：卸下
        for device in &imported {
            if !self
                .identities
                .iter()
                .any(|identity| identity.busid.as_str() == device.busid)
            {
                info!(port = %device.port, busid = %device.busid, "detaching a stray Aemeath device");
                if let Err(error) = self.cli.detach(device.port).await {
                    warn!(%error, port = %device.port, "cannot detach");
                }
            }
        }

        // 第一遍：不需要端点信息就能判定的槽位（拔出、未导入、重复导入、服务端没有连接）立即处理。
        // 设备正在增减时，查询端点可能被系统阻塞好几秒（实测引擎被结束后约 5 s），不能让修复等它。
        let mut needs_endpoint = Vec::new();
        for identity in self.identities.clone() {
            let ports: Vec<VhciPort> = imported
                .iter()
                .filter(|device| device.busid == identity.busid.as_str())
                .map(|device| device.port)
                .collect();
            let connected = self
                .slots
                .get(&identity.slot)
                .is_some_and(|state| state.connected_since.is_some());
            if self.occupied.contains(&identity.slot) && ports.len() == 1 && connected {
                needs_endpoint.push((identity, ports));
            } else {
                let endpoint = self.known_endpoints.get(&identity.slot).cloned();
                self.check_slot(&identity, &ports, endpoint).await;
            }
        }

        // 第二遍：查询端点，判定已连接的槽位是否正常；同时刷新其他槽位的端点信息（设备页显示格式）
        match tokio::time::timeout(self.timing.endpoint_query_timeout, self.probe.snapshot()).await {
            Ok(Ok(endpoints)) => self.known_endpoints = endpoints,
            Ok(Err(error)) => {
                warn!(%error, "cannot query the slot endpoints");
                return;
            }
            Err(_) => {
                debug!("the endpoint query is slow (devices are changing), checking again later");
                return;
            }
        }
        for (identity, ports) in needs_endpoint {
            let endpoint = self.known_endpoints.get(&identity.slot).cloned();
            self.check_slot(&identity, &ports, endpoint).await;
        }
        for (slot, state) in &self.slots {
            if let Some((health, _)) = &state.reported {
                let refresh = (*health, self.known_endpoints.get(slot).cloned());
                if state.reported.as_ref() != Some(&refresh) {
                    self.pending_refresh.push((*slot, refresh.0, refresh.1));
                }
            }
        }
        for (slot, health, endpoint) in std::mem::take(&mut self.pending_refresh) {
            self.report(slot, health, endpoint);
        }
    }

    async fn check_slot(&mut self, identity: &SlotIdentity, ports: &[VhciPort], endpoint: Option<SlotEndpoint>) {
        let slot = identity.slot;
        let Some(state) = self.slots.get(&slot) else {
            return;
        };
        let observation = Observation {
            occupied: self.occupied.contains(&slot),
            ports,
            server_connected: state.connected_since.is_some(),
            endpoint: endpoint.as_ref().map(|endpoint| endpoint.state),
            connected_for: state.connected_since.map(|since| since.elapsed()),
        };
        match assess(&observation, self.timing.endpoint_grace) {
            Assessment::Unplugged => self.report(slot, SlotHealth::Unplugged, endpoint),
            Assessment::Unplug => {
                info!(%slot, ?ports, "unplugging an unoccupied slot");
                self.clear(identity, ports).await;
                self.report(slot, SlotHealth::Unplugged, endpoint);
            }
            Assessment::Settling => self.report(slot, SlotHealth::Settling, endpoint),
            Assessment::Healthy => {
                if let Some(state) = self.slots.get_mut(&slot) {
                    if state.failures > 0 {
                        info!(%slot, failures = state.failures, "slot recovered");
                    }
                    state.failures = 0;
                    state.last_failure = None;
                    state.retry_at = None;
                }
                let endpoint = self.ensure_format(slot, endpoint).await;
                self.report(slot, SlotHealth::Healthy, endpoint);
            }
            Assessment::Repair(reason) => {
                let waiting = state.retry_at.is_some_and(|retry_at| Instant::now() < retry_at);
                if waiting {
                    return;
                }
                let attached = self.repair(identity, ports, reason).await;
                let endpoint = endpoint.filter(|endpoint| endpoint.state == EndpointState::Active);
                if let Some(state) = self.slots.get(&slot) {
                    let health = if attached {
                        SlotHealth::Settling
                    } else {
                        health_after_failure(state, reason)
                    };
                    self.report(slot, health, endpoint);
                }
            }
        }
    }

    /// 停掉这台设备的后台重试，卸下它的全部条目。
    async fn clear(&self, identity: &SlotIdentity, ports: &[VhciPort]) {
        if let Err(error) = self.cli.stop_retry(&identity.busid).await {
            warn!(%error, slot = %identity.slot, "cannot stop the pending retry");
        }
        for port in ports {
            if let Err(error) = self.cli.detach(*port).await {
                warn!(%error, slot = %identity.slot, %port, "cannot detach");
            }
        }
    }

    /// 返回是否挂上（服务端收到了 import）。
    async fn repair(&mut self, identity: &SlotIdentity, ports: &[VhciPort], reason: RepairReason) -> bool {
        let slot = identity.slot;
        info!(%slot, ?reason, "repairing slot");
        self.clear(identity, ports).await;
        let attached = match self.cli.attach(&identity.busid).await {
            Ok(port) => {
                debug!(%slot, %port, "attach command succeeded, waiting for the import");
                self.wait_for_connection(slot).await
            }
            Err(error) => {
                warn!(%slot, %error, "attach failed");
                false
            }
        };
        let Some(state) = self.slots.get_mut(&slot) else {
            return attached;
        };
        if attached {
            info!(%slot, "slot attached");
            state.retry_at = None;
            return true;
        }
        state.failures += 1;
        state.last_failure = Some(reason);
        let delay = RETRY_DELAYS[(state.failures as usize - 1).min(RETRY_DELAYS.len() - 1)];
        state.retry_at = Some(Instant::now() + delay);
        warn!(%slot, failures = state.failures, retry_in = ?delay, "slot repair failed");
        false
    }

    /// 等服务端报告收到 import（ADR-0007 的成功判据）。期间照常处理其他输入。
    async fn wait_for_connection(&mut self, slot: SlotId) -> bool {
        let deadline = Instant::now() + self.timing.attach_confirm;
        loop {
            if self
                .slots
                .get(&slot)
                .is_some_and(|state| state.connected_since.is_some())
            {
                return true;
            }
            if !self.engine_up {
                return false;
            }
            match tokio::time::timeout_at(deadline, self.inputs.recv()).await {
                Ok(Some(input)) => self.apply(input),
                Ok(None) | Err(_) => return false,
            }
        }
    }

    /// 端点首次变为 ACTIVE 时核对采样率，与引擎不同就改成引擎采样率。
    async fn ensure_format(&mut self, slot: SlotId, endpoint: Option<SlotEndpoint>) -> Option<SlotEndpoint> {
        let state = self.slots.get_mut(&slot)?;
        let mut endpoint = endpoint?;
        if state.format_checked {
            return Some(endpoint);
        }
        state.format_checked = true;
        let matches = endpoint.format.is_some_and(|format| format.sample_rate == self.rate);
        if matches {
            return Some(endpoint);
        }
        info!(%slot, current = ?endpoint.format, engine = self.rate.hz(), "setting the slot sample rate");
        match self.probe.set_rate(endpoint.id.clone(), self.rate).await {
            Ok(format) => endpoint.format = Some(format),
            Err(error) => warn!(%slot, %error, "cannot set the slot sample rate"),
        }
        Some(endpoint)
    }

    fn report(&mut self, slot: SlotId, health: SlotHealth, endpoint: Option<SlotEndpoint>) {
        let Some(state) = self.slots.get_mut(&slot) else {
            return;
        };
        let current = (health, endpoint);
        if state.reported.as_ref() == Some(&current) {
            return;
        }
        debug!(%slot, health = ?current.0, "slot status");
        let (health, endpoint) = current.clone();
        state.reported = Some(current);
        let _ = self.statuses.send(SlotStatus { slot, health, endpoint });
    }
}

/// 修复失败后的健康状态：按连续失败次数决定是否提示用户。
fn health_after_failure(state: &SlotState, reason: RepairReason) -> SlotHealth {
    let reason = state.last_failure.unwrap_or(reason);
    let failures = state.failures;
    if failures >= ALERT_AFTER_FAILURES {
        SlotHealth::Failing { failures, reason }
    } else {
        SlotHealth::Retrying { failures, reason }
    }
}
