//! 控制面（ADR-0003）：工作台、配置文件、与引擎的 IPC、槽位的挂载维护与首次注册。
//!
//! 线程模型：一个 actor 任务独占全部状态，界面的命令和引擎、槽位的事件都经通道送进来依次处理，不加锁。
//! 每次编辑后：保存配置、把编译出的路由拓扑交给 IPC 客户端（拓扑没变时不下发）、更新被占用的槽位。
//!
//! 首次启动（配置里还没有"已注册"）：先下发注册用拓扑、占用全部 16 个槽位，等全部完成后再换回工作台
//! （ADR-0009）。工作台一开始是空的；工作台为空时界面提供预设（GLOSSARY：预设），节点名由界面按当前语言提供。

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::time::{Duration, SystemTime};

use aemeath_control::audio_policy::AudioPolicy;
use aemeath_control::engine_client::{ClientConfig, ClientEvent, EngineClient, EngineProber, Timing};
use aemeath_control::slots::endpoints::{EndpointProbe, ExternalDevice};
use aemeath_control::slots::registration::{RegistrationProgress, all_slots, registration_topology};
use aemeath_control::slots::{MaintainerConfig, MaintainerTiming, SlotMaintainer, SlotStatus};
use aemeath_control::usbip_cli::UsbipCli;
use aemeath_core::model::{DeviceRef, LabelPosition, Node, NodeKind, NodeName, Position};
use aemeath_core::preferences::LanguagePreference;
use aemeath_core::store::{Config, ConfigStore, Loaded};
use aemeath_core::workspace::{EditError, NodeSpec, Workspace};
use aemeath_protocol::ids::{EndpointId, NodeId, SlotId, SlotNumber, TopologyRevision};
use aemeath_protocol::message::{EndpointDirection, EndpointStatus, OfflineReason};
use aemeath_protocol::topology::{DeviceSettings, NodeRole, RouteRejection, RouteTopology, Strip};
use aemeath_protocol::units::{GainDb, GateThreshold, SampleRate};
use aemeath_runtime::config::RuntimeConfig;
use aemeath_sys::audio_endpoints::AudioFlow;
use aemeath_sys::policy_config::DefaultRole;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tracing::{error, info, warn};

use crate::apps::{self, AppsView};
use crate::default_devices::DefaultDevicesView;
use crate::device_settings::{CapabilitiesView, DeviceSettingsView};
use crate::view::{
    DeviceView, EngineState, EngineView, FormatView, NodeStatus, NodeView, NoticeKind, NoticeView, PairVerdict,
    RegistrationView, RouteVerdict, RouteView, SlotRef, SlotView, ViewState, kind_view,
};

/// 撤销最多保留的步数
const UNDO_LIMIT: usize = 50;
/// 端点变化后等这么久再做后备匹配，让成批的变化通知都到齐
const REMATCH_SETTLE: Duration = Duration::from_millis(300);
/// 事件密集到达时（例如注册时 16 个槽位同时变化），合并成一次推送
const EMIT_COALESCE: Duration = Duration::from_millis(50);
/// 检查系统是否刚从睡眠唤醒的间隔
const RESUME_PROBE_INTERVAL: Duration = Duration::from_secs(2);
/// 墙上时钟比预期多走这么多，就认为系统刚睡眠过（ADR-0007：唤醒后立即检查挂载）
const RESUME_GAP: Duration = Duration::from_secs(10);
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 命令失败的原因（界面据此显示本地化的提示）。
#[derive(Debug, Clone, Serialize, Error)]
#[serde(tag = "code", rename_all = "camelCase")]
pub enum CommandError {
    #[error("the control plane is not running")]
    Unavailable,
    #[error("the node name is empty or too long")]
    InvalidName,
    #[error("the route is not allowed")]
    RouteRejected { rejection: crate::view::RejectionView },
    #[error("all slots of this direction are in use")]
    NoFreeSlot,
    #[error("the slot is in use")]
    SlotOccupied,
    #[error("this device already has a node")]
    DeviceInUse,
    #[error("the node or route no longer exists")]
    NotFound,
    #[error("{message}")]
    Failed { message: String },
}

impl From<EditError> for CommandError {
    fn from(error: EditError) -> Self {
        match error {
            EditError::UnknownNode(_) | EditError::UnknownRoute { .. } | EditError::NotAVirtualNode(_) => {
                Self::NotFound
            }
            EditError::NoFreeSlot(_) => Self::NoFreeSlot,
            EditError::SlotOccupied { .. } => Self::SlotOccupied,
            EditError::DeviceInUse { .. } => Self::DeviceInUse,
            EditError::Route(rejection) => Self::RouteRejected {
                rejection: rejection.into(),
            },
            EditError::InvalidClockMaster | EditError::GateNotAllowed(_) | EditError::NotAnExternalNode(_) => {
                Self::Failed {
                    message: error.to_string(),
                }
            }
        }
    }
}

/// 新建节点的类型（界面的"添加节点"）。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum NewNodeKind {
    #[serde(rename_all = "camelCase")]
    ExternalInput {
        endpoint_id: String,
        device_name: String,
    },
    #[serde(rename_all = "camelCase")]
    ExternalOutput {
        endpoint_id: String,
        device_name: String,
    },
    VirtualInput,
    VirtualOutput,
    Bus,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeviceDirection {
    Input,
    Output,
}

/// 界面发起的编辑。
#[derive(Debug, Clone)]
pub enum Edit {
    /// 使用预设：在空工作台上添加一个虚拟输入（In 1）和一个虚拟输出（Out 1），不带连线。
    /// 名称由界面按当前语言提供；工作台不为空时不做任何事
    ApplyPreset {
        input_name: String,
        output_name: String,
    },
    AddNode {
        kind: NewNodeKind,
        name: String,
        position: Position,
        /// 外部设备节点在添加时就设定的设备设置（ADR-0014）；None 表示默认值
        io: Option<DeviceSettings>,
    },
    /// 外部设备节点的独占、格式、缓冲和重采样算法（节点设置）
    SetDeviceSettings {
        node: NodeId,
        settings: DeviceSettings,
    },
    /// 用户从同名候选里选定外部设备节点要改用的端点
    ReplaceDevice {
        node: NodeId,
        endpoint_id: String,
        device_name: String,
    },
    RemoveNode(NodeId),
    RenameNode {
        node: NodeId,
        name: String,
    },
    MoveNode {
        node: NodeId,
        position: Position,
    },
    /// 虚拟节点改占同方向的另一个槽位（节点设置，ADR-0009）
    ChangeSlot {
        node: NodeId,
        number: u8,
    },
    Connect {
        from: NodeId,
        to: NodeId,
    },
    Disconnect {
        from: NodeId,
        to: NodeId,
    },
    SetRouteEnabled {
        from: NodeId,
        to: NodeId,
        enabled: bool,
    },
    SetRouteGain {
        from: NodeId,
        to: NodeId,
        gain_db: f32,
    },
    /// 连线标签在连线上的位置（0–1）
    SetRouteLabel {
        from: NodeId,
        to: NodeId,
        position: f32,
    },
    SetNodeGain {
        node: NodeId,
        gain_db: f32,
    },
    SetNodeMuted {
        node: NodeId,
        muted: bool,
    },
    /// 门限阈值；None 表示关
    SetNodeGate {
        node: NodeId,
        threshold_db: Option<f32>,
    },
    /// 固定到控制台（GLOSSARY：固定）
    Pin(NodeId),
    Unpin(NodeId),
    /// 在控制台上把节点移到第 `index` 个位置
    MovePin {
        node: NodeId,
        index: usize,
    },
    SetLanguage(LanguagePreference),
    Undo,
}

impl Edit {
    /// 结构性的修改才进入撤销记录；移动、增益、标签位置这类连续调整不进入
    fn is_undoable(&self) -> bool {
        matches!(
            self,
            Self::AddNode { .. }
                | Self::ApplyPreset { .. }
                | Self::RemoveNode(_)
                | Self::RenameNode { .. }
                | Self::ChangeSlot { .. }
                | Self::SetDeviceSettings { .. }
                | Self::ReplaceDevice { .. }
                | Self::Connect { .. }
                | Self::Disconnect { .. }
                | Self::SetRouteEnabled { .. }
                | Self::SetNodeMuted { .. }
        )
    }
}

enum Request {
    State(oneshot::Sender<ViewState>),
    Edit {
        edit: Edit,
        reply: oneshot::Sender<Result<ViewState, CommandError>>,
    },
    Verdicts {
        node: NodeId,
        from_output: bool,
        reply: oneshot::Sender<Vec<RouteVerdict>>,
    },
    MatrixVerdicts(oneshot::Sender<Vec<PairVerdict>>),
    Apps(oneshot::Sender<Result<AppsView, CommandError>>),
    SetLanding {
        process_id: u32,
        executable: Option<String>,
        endpoint_id: Option<String>,
        reply: oneshot::Sender<Result<(), CommandError>>,
    },
    DefaultDevices(oneshot::Sender<Result<DefaultDevicesView, CommandError>>),
    SetDefaultDevice {
        endpoint_id: String,
        role: DefaultRole,
        reply: oneshot::Sender<Result<(), CommandError>>,
    },
    /// 设备探测的句柄和当前的引擎采样率（探测在 actor 之外等待应答，不阻塞其他请求）
    Prober(oneshot::Sender<(EngineProber, SampleRate)>),
    Redetect(Option<NodeId>),
    Devices {
        direction: DeviceDirection,
        reply: oneshot::Sender<Result<Vec<ExternalDevice>, CommandError>>,
    },
    CheckSlots,
    ShutdownEngine(oneshot::Sender<bool>),
}

/// 控制面的句柄，由 Tauri 命令共享。
#[derive(Debug, Clone)]
pub struct ControllerHandle {
    requests: mpsc::UnboundedSender<Request>,
}

impl ControllerHandle {
    /// 启动控制面。`emit` 在状态变化时调用（推给界面）；在 Tauri 的异步运行时上运行。
    pub fn start(config: RuntimeConfig, emit: impl Fn(&ViewState) + Send + 'static) -> Self {
        let (requests, receiver) = mpsc::unbounded_channel();
        tauri::async_runtime::spawn(async move {
            match Controller::new(config, Box::new(emit)).await {
                Ok(controller) => controller.run(receiver).await,
                Err(error) => error!(%error, "the control plane cannot start"),
            }
        });
        Self { requests }
    }

    async fn ask<T>(&self, request: impl FnOnce(oneshot::Sender<T>) -> Request) -> Result<T, CommandError> {
        let (reply, wait) = oneshot::channel();
        self.requests
            .send(request(reply))
            .map_err(|_| CommandError::Unavailable)?;
        wait.await.map_err(|_| CommandError::Unavailable)
    }

    pub async fn state(&self) -> Result<ViewState, CommandError> {
        self.ask(Request::State).await
    }

    pub async fn edit(&self, edit: Edit) -> Result<ViewState, CommandError> {
        self.ask(|reply| Request::Edit { edit, reply }).await?
    }

    pub async fn verdicts(&self, node: NodeId, from_output: bool) -> Result<Vec<RouteVerdict>, CommandError> {
        self.ask(|reply| Request::Verdicts {
            node,
            from_output,
            reply,
        })
        .await
    }

    /// 查询外部设备的能力（添加节点和节点设置用，ADR-0014）。引擎不在或设备打不开时返回 None。
    pub async fn probe_device(
        &self,
        endpoint: EndpointId,
        direction: DeviceDirection,
    ) -> Result<Option<CapabilitiesView>, CommandError> {
        let (prober, rate) = self.ask(Request::Prober).await?;
        let direction = match direction {
            DeviceDirection::Input => EndpointDirection::Input,
            DeviceDirection::Output => EndpointDirection::Output,
        };
        Ok(prober
            .probe_device(endpoint, direction)
            .await
            .map(|capabilities| CapabilitiesView::new(&capabilities, rate)))
    }

    /// 应用页：正在出声的程序与可选的落点。
    pub async fn apps(&self) -> Result<AppsView, CommandError> {
        self.ask(Request::Apps).await?
    }

    /// 把程序的落点指派为 `endpoint_id`（None 表示跟随系统默认），并记下这次指派。
    pub async fn set_landing(
        &self,
        process_id: u32,
        executable: Option<String>,
        endpoint_id: Option<String>,
    ) -> Result<(), CommandError> {
        self.ask(|reply| Request::SetLanding {
            process_id,
            executable,
            endpoint_id,
            reply,
        })
        .await?
    }

    /// 工具页：两个方向的活动端点及 Windows 默认设备。
    pub async fn default_devices(&self) -> Result<DefaultDevicesView, CommandError> {
        self.ask(Request::DefaultDevices).await?
    }

    pub async fn set_default_device(&self, endpoint_id: String, role: DefaultRole) -> Result<(), CommandError> {
        self.ask(|reply| Request::SetDefaultDevice {
            endpoint_id,
            role,
            reply,
        })
        .await?
    }

    /// 立即重新检测离线的外部设备；`node` 为 None 表示全部（节点菜单、托盘、设置页）。
    pub fn redetect(&self, node: Option<NodeId>) {
        let _ = self.requests.send(Request::Redetect(node));
    }

    /// 路由矩阵里不能新建连线的格子（连到自己、会形成回路），以及原因。已有连线的格子不在其中。
    pub async fn matrix_verdicts(&self) -> Result<Vec<PairVerdict>, CommandError> {
        self.ask(Request::MatrixVerdicts).await
    }

    pub async fn devices(&self, direction: DeviceDirection) -> Result<Vec<ExternalDevice>, CommandError> {
        self.ask(|reply| Request::Devices { direction, reply }).await?
    }

    pub fn check_slots(&self) {
        let _ = self.requests.send(Request::CheckSlots);
    }

    /// 请引擎退出（用户从托盘退出时）。返回引擎是否确认。
    pub async fn shutdown_engine(&self) -> bool {
        self.ask(Request::ShutdownEngine).await.unwrap_or(false)
    }
}

#[derive(Debug, Error)]
enum StartError {
    #[error("cannot start the endpoint thread: {0}")]
    Endpoints(String),
    #[error("the slot identity table is invalid: {0}")]
    Identity(String),
}

struct Controller {
    config: Config,
    store: ConfigStore,
    /// 配置文件是更新版本写的：只在内存中使用，不覆盖它
    read_only: bool,
    undo: Vec<Workspace>,
    client: EngineClient,
    client_events: mpsc::UnboundedReceiver<ClientEvent>,
    maintainer: Option<SlotMaintainer>,
    slot_events: mpsc::UnboundedReceiver<SlotStatus>,
    probe: Option<EndpointProbe>,
    /// 应用会话、按应用指派与 Windows 默认设备；启动失败时应用页和工具页不可用
    policy: Option<AudioPolicy>,
    usbip_version: Option<String>,
    engine_state: EngineState,
    endpoint_status: HashMap<NodeId, EndpointStatus>,
    slot_status: BTreeMap<SlotId, SlotStatus>,
    registration: Option<RegistrationProgress>,
    last_topology: Option<RouteTopology>,
    /// 触发一次按名称的后备匹配（端点增减时、节点报告"等待设备"时）
    rematch_trigger: mpsc::UnboundedSender<()>,
    rematch_events: mpsc::UnboundedReceiver<()>,
    /// 找到多个同名候选、等用户选择的节点
    candidates: HashMap<NodeId, Vec<ExternalDevice>>,
    /// 给界面的一次性提示，界面按编号只显示新的
    notices: VecDeque<NoticeView>,
    next_notice: u64,
    emit: Box<dyn Fn(&ViewState) + Send>,
    emit_at: Option<Instant>,
}

/// 最多保留的提示条数（界面只显示没见过的）
const NOTICE_LIMIT: usize = 8;
/// 预设节点在工作台上的位置：输入在左、输出在右，中间留出连线的空间
const PRESET_INPUT_AT: Position = Position { x: 40.0, y: 60.0 };
const PRESET_OUTPUT_AT: Position = Position { x: 440.0, y: 60.0 };

impl Controller {
    async fn new(runtime: RuntimeConfig, emit: Box<dyn Fn(&ViewState) + Send>) -> Result<Self, StartError> {
        let store = ConfigStore::new(&runtime.data_dir);
        let (config, read_only) = match store.load() {
            Ok(Loaded::Existing(config)) => (config, false),
            Ok(Loaded::Missing) => {
                info!("no configuration yet, starting with an empty workspace");
                (Config::default(), false)
            }
            Ok(Loaded::Recovered { config, backup, reason }) => {
                warn!(%reason, backup = %backup.display(), "the configuration was invalid and has been backed up");
                (config, false)
            }
            Err(error) => {
                error!(%error, "cannot use the configuration file, changes will not be saved");
                (Config::default(), true)
            }
        };
        let rate = config.workspace.engine.sample_rate;

        let (client_tx, client_events) = mpsc::unbounded_channel();
        let client = EngineClient::start(
            ClientConfig {
                pipe_name: runtime.pipe_name.clone(),
                app_version: APP_VERSION.to_owned(),
                timing: Timing::default(),
            },
            client_tx,
        );

        let usbip = UsbipCli::default_program().map(|program| UsbipCli::new(program, runtime.usbip_port));
        let usbip_version = match &usbip {
            Some(cli) => match cli.version().await {
                Ok(version) => Some(version.to_string()),
                Err(error) => {
                    warn!(%error, "cannot read the usbip-win2 version");
                    None
                }
            },
            None => None,
        };

        let (slot_tx, slot_events) = mpsc::unbounded_channel();
        let maintainer = match usbip {
            Some(cli) => match SlotMaintainer::start(
                MaintainerConfig {
                    identity: runtime.identity,
                    cli,
                    engine_rate: rate,
                    timing: MaintainerTiming::default(),
                },
                slot_tx,
            ) {
                Ok(maintainer) => Some(maintainer),
                Err(error) => {
                    error!(%error, "cannot start the slot maintenance");
                    None
                }
            },
            None => {
                error!("%ProgramFiles% is not set, cannot find usbip.exe");
                None
            }
        };
        let identities = runtime
            .identity
            .all()
            .map_err(|error| StartError::Identity(error.to_string()))?;
        let (rematch_trigger, rematch_events) = mpsc::unbounded_channel();
        let on_endpoint_change = rematch_trigger.clone();
        let probe = EndpointProbe::start(identities, move || {
            let _ = on_endpoint_change.send(());
        })
        .map_err(|error| StartError::Endpoints(error.to_string()))?;

        let registration = (!config.preferences.slots_registered).then(|| {
            info!("the slots are not registered yet, registering all 16");
            RegistrationProgress::new(rate)
        });
        let mut controller = Self {
            config,
            store,
            read_only,
            undo: Vec::new(),
            client,
            client_events,
            maintainer,
            slot_events,
            probe: Some(probe),
            policy: AudioPolicy::start()
                .inspect_err(|error| warn!(%error, "the audio policy service cannot start"))
                .ok(),
            usbip_version,
            engine_state: EngineState::Connecting,
            endpoint_status: HashMap::new(),
            slot_status: BTreeMap::new(),
            registration,
            last_topology: None,
            rematch_trigger,
            rematch_events,
            candidates: HashMap::new(),
            notices: VecDeque::new(),
            next_notice: 1,
            emit,
            emit_at: None,
        };
        controller.sync_engine();
        Ok(controller)
    }

    async fn run(mut self, mut requests: mpsc::UnboundedReceiver<Request>) {
        let mut resume_probe = tokio::time::interval(RESUME_PROBE_INTERVAL);
        let mut last_probe = SystemTime::now();
        loop {
            let emit_at = self.emit_at;
            tokio::select! {
                request = requests.recv() => {
                    let Some(request) = request else {
                        return;
                    };
                    self.handle(request).await;
                }
                Some(event) = self.client_events.recv() => self.on_engine_event(event),
                Some(status) = self.slot_events.recv() => self.on_slot_status(status),
                Some(()) = self.rematch_events.recv() => {
                    // 端点变化往往成批到达（一台设备有多个端点）：合并成一次匹配
                    tokio::time::sleep(REMATCH_SETTLE).await;
                    while self.rematch_events.try_recv().is_ok() {}
                    self.rematch().await;
                }
                () = sleep_until(emit_at), if emit_at.is_some() => {
                    self.emit_at = None;
                    (self.emit)(&self.view());
                }
                _ = resume_probe.tick() => {
                    let now = SystemTime::now();
                    let gap = now.duration_since(last_probe).unwrap_or_default();
                    last_probe = now;
                    if gap > RESUME_PROBE_INTERVAL + RESUME_GAP {
                        info!(gap = ?gap, "the system seems to have resumed from sleep, checking the slots");
                        if let Some(maintainer) = &self.maintainer {
                            maintainer.check_now();
                        }
                    }
                }
            }
        }
    }

    async fn handle(&mut self, request: Request) {
        match request {
            Request::State(reply) => {
                let _ = reply.send(self.view());
            }
            Request::Edit { edit, reply } => {
                let result = self.apply(edit).map(|()| self.view());
                let _ = reply.send(result);
            }
            Request::Verdicts {
                node,
                from_output,
                reply,
            } => {
                let _ = reply.send(self.verdicts(node, from_output));
            }
            Request::MatrixVerdicts(reply) => {
                let _ = reply.send(self.matrix_verdicts());
            }
            Request::Prober(reply) => {
                let _ = reply.send((self.client.prober(), self.config.workspace.engine.sample_rate));
            }
            Request::Redetect(node) => self.client.redetect(node),
            Request::Apps(reply) => {
                let _ = reply.send(self.apps().await);
            }
            Request::SetLanding {
                process_id,
                executable,
                endpoint_id,
                reply,
            } => {
                let _ = reply.send(self.set_landing(process_id, executable, endpoint_id).await);
            }
            Request::DefaultDevices(reply) => {
                let _ = reply.send(self.default_devices().await);
            }
            Request::SetDefaultDevice {
                endpoint_id,
                role,
                reply,
            } => {
                let result = match &self.policy {
                    Some(policy) => policy.set_default(endpoint_id, role).await.map_err(failed),
                    None => Err(CommandError::Unavailable),
                };
                let _ = reply.send(result);
            }
            Request::Devices { direction, reply } => {
                let flow = match direction {
                    DeviceDirection::Input => AudioFlow::Capture,
                    DeviceDirection::Output => AudioFlow::Render,
                };
                let result = match &self.probe {
                    Some(probe) => probe
                        .external_devices(flow)
                        .await
                        .map_err(|error| CommandError::Failed {
                            message: error.to_string(),
                        }),
                    None => Err(CommandError::Unavailable),
                };
                let _ = reply.send(result);
            }
            Request::CheckSlots => {
                if let Some(maintainer) = &self.maintainer {
                    maintainer.check_now();
                }
            }
            Request::ShutdownEngine(reply) => {
                let _ = reply.send(self.client.shutdown_engine().await);
            }
        }
    }

    fn apply(&mut self, edit: Edit) -> Result<(), CommandError> {
        let before = self.config.workspace.clone();
        let undoable = edit.is_undoable();
        let workspace = &mut self.config.workspace;
        match edit {
            Edit::ApplyPreset {
                input_name,
                output_name,
            } => {
                if workspace.nodes().is_empty() {
                    let input = NodeName::new(&input_name).map_err(|_| CommandError::InvalidName)?;
                    let output = NodeName::new(&output_name).map_err(|_| CommandError::InvalidName)?;
                    let first = SlotNumber::new(SlotNumber::FIRST).ok();
                    workspace.add_node(input, NodeSpec::VirtualInput(first), PRESET_INPUT_AT)?;
                    workspace.add_node(output, NodeSpec::VirtualOutput(first), PRESET_OUTPUT_AT)?;
                }
            }
            Edit::SetDeviceSettings { node, settings } => workspace.set_device_settings(node, settings)?,
            Edit::ReplaceDevice {
                node,
                endpoint_id,
                device_name,
            } => {
                workspace.replace_device(
                    node,
                    DeviceRef {
                        endpoint: EndpointId(endpoint_id),
                        name: device_name,
                    },
                )?;
                self.candidates.remove(&node);
            }
            Edit::AddNode {
                kind,
                name,
                position,
                io,
            } => {
                let name = NodeName::new(&name).map_err(|_| CommandError::InvalidName)?;
                let device = |endpoint_id: String, device_name: String| DeviceRef {
                    endpoint: EndpointId(endpoint_id),
                    name: device_name,
                };
                let spec = match kind {
                    NewNodeKind::ExternalInput {
                        endpoint_id,
                        device_name,
                    } => NodeSpec::ExternalInput(device(endpoint_id, device_name)),
                    NewNodeKind::ExternalOutput {
                        endpoint_id,
                        device_name,
                    } => NodeSpec::ExternalOutput(device(endpoint_id, device_name)),
                    NewNodeKind::VirtualInput => NodeSpec::VirtualInput(None),
                    NewNodeKind::VirtualOutput => NodeSpec::VirtualOutput(None),
                    NewNodeKind::Bus => NodeSpec::Bus,
                };
                let node = workspace.add_node(name, spec, position)?;
                if let Some(settings) = io {
                    workspace.set_device_settings(node, settings)?;
                }
            }
            Edit::RemoveNode(node) => {
                workspace.remove_node(node)?;
            }
            Edit::RenameNode { node, name } => {
                let name = NodeName::new(&name).map_err(|_| CommandError::InvalidName)?;
                workspace.rename_node(node, name)?;
            }
            Edit::MoveNode { node, position } => workspace.move_node(node, position)?,
            Edit::ChangeSlot { node, number } => {
                let number = SlotNumber::new(number).map_err(|_| CommandError::NotFound)?;
                workspace.change_slot(node, number)?;
            }
            Edit::Connect { from, to } => workspace.connect(from, to)?,
            Edit::Disconnect { from, to } => {
                workspace.disconnect(from, to)?;
            }
            Edit::SetRouteEnabled { from, to, enabled } => workspace.set_route_enabled(from, to, enabled)?,
            Edit::SetRouteGain { from, to, gain_db } => {
                let gain = GainDb::new(gain_db).map_err(|error| CommandError::Failed {
                    message: error.to_string(),
                })?;
                workspace.set_route_gain(from, to, gain)?;
            }
            Edit::SetRouteLabel { from, to, position } => {
                workspace.set_route_label(from, to, LabelPosition::new(position))?;
            }
            Edit::SetNodeGain { node, gain_db } => {
                let gain = GainDb::new(gain_db).map_err(|error| CommandError::Failed {
                    message: error.to_string(),
                })?;
                let strip = Strip {
                    gain,
                    ..strip_of(workspace, node)?
                };
                workspace.set_strip(node, strip)?;
            }
            Edit::SetNodeMuted { node, muted } => {
                let strip = Strip {
                    muted,
                    ..strip_of(workspace, node)?
                };
                workspace.set_strip(node, strip)?;
            }
            Edit::SetNodeGate { node, threshold_db } => {
                let gate = threshold_db
                    .map(GateThreshold::new)
                    .transpose()
                    .map_err(|error| CommandError::Failed {
                        message: error.to_string(),
                    })?;
                let strip = Strip {
                    gate,
                    ..strip_of(workspace, node)?
                };
                workspace.set_strip(node, strip)?;
            }
            Edit::Pin(node) => workspace.pin(node)?,
            Edit::Unpin(node) => workspace.unpin(node),
            Edit::MovePin { node, index } => workspace.move_pin(node, index)?,
            Edit::SetLanguage(language) => self.config.preferences.language = language,
            Edit::Undo => match self.undo.pop() {
                Some(snapshot) => workspace.restore(snapshot),
                None => return Ok(()),
            },
        }
        if undoable && self.config.workspace != before {
            self.undo.push(before);
            if self.undo.len() > UNDO_LIMIT {
                self.undo.remove(0);
            }
        }
        self.save();
        self.sync_engine();
        Ok(())
    }

    fn save(&self) {
        if self.read_only {
            return;
        }
        if let Err(error) = self.store.save(&self.config) {
            error!(%error, "cannot save the configuration");
        }
    }

    /// 把当前应执行的路由拓扑和被占用的槽位交给引擎与挂载维护。拓扑没变时不重复下发。
    fn sync_engine(&mut self) {
        let rate = self.config.workspace.engine.sample_rate;
        let (topology, occupied): (RouteTopology, BTreeSet<SlotId>) = if self.registration.is_some() {
            (registration_topology(rate), all_slots())
        } else {
            let workspace = &self.config.workspace;
            (
                workspace.compile(TopologyRevision(0)),
                workspace.occupied_slots().collect(),
            )
        };
        if self.last_topology.as_ref() != Some(&topology) {
            self.client.apply(topology.clone());
            self.last_topology = Some(topology);
        }
        if let Some(maintainer) = &self.maintainer {
            maintainer.set_engine_rate(rate);
            maintainer.set_occupied(occupied);
        }
    }

    fn on_engine_event(&mut self, event: ClientEvent) {
        if let Some(maintainer) = &self.maintainer {
            maintainer.engine_event(&event);
        }
        match event {
            ClientEvent::Connected { .. } => self.engine_state = EngineState::Running,
            ClientEvent::Disconnected(reason) => {
                self.engine_state = EngineState::after_disconnect(&reason);
                self.endpoint_status.clear();
            }
            ClientEvent::EndpointStatus { node, status } => {
                if let EndpointStatus::Offline {
                    reason: OfflineReason::WaitingForDevice,
                } = status
                {
                    let _ = self.rematch_trigger.send(());
                }
                self.endpoint_status.insert(node, status);
            }
            ClientEvent::TopologyRejected {
                revision,
                reason,
                detail,
            } => {
                error!(revision = revision.0, ?reason, %detail, "the engine rejected the topology");
            }
            ClientEvent::TopologyApplied { .. }
            | ClientEvent::SlotConnection { .. }
            | ClientEvent::SlotFormat { .. } => {
                return;
            }
        }
        self.schedule_emit();
    }

    fn on_slot_status(&mut self, status: SlotStatus) {
        if let Some(progress) = &mut self.registration {
            progress.update(status.clone());
            if progress.is_done() {
                info!("all 16 slots are registered");
                self.registration = None;
                self.config.preferences.slots_registered = true;
                self.save();
                self.sync_engine();
            }
        }
        self.slot_status.insert(status.slot, status);
        self.schedule_emit();
    }

    /// 按名称的后备匹配（选型报告 6.5 节，2026-11-08 维护者决定）：等待设备的外部节点，端点 ID 已不在系统中时，
    /// 在同方向的活动端点里找同名、且没有被其他节点使用的设备。只有一个时自动改用并提示；有多个时等用户选。
    async fn rematch(&mut self) {
        let Some(probe) = &self.probe else {
            return;
        };
        let waiting: Vec<(NodeId, DeviceRef, AudioFlow)> = self
            .config
            .workspace
            .nodes()
            .iter()
            .filter(|node| {
                matches!(
                    self.endpoint_status.get(&node.id),
                    Some(EndpointStatus::Offline {
                        reason: OfflineReason::WaitingForDevice
                    })
                )
            })
            .filter_map(|node| {
                let flow = match node.kind {
                    NodeKind::ExternalInput { .. } => AudioFlow::Capture,
                    NodeKind::ExternalOutput { .. } => AudioFlow::Render,
                    _ => return None,
                };
                Some((node.id, node.kind.device()?.clone(), flow))
            })
            .collect();
        // 先取齐需要的设备列表，之后才修改自身状态
        let mut lists: HashMap<AudioFlow, Vec<ExternalDevice>> = HashMap::new();
        for (_, _, flow) in &waiting {
            if !lists.contains_key(flow) {
                let list = probe.external_devices(*flow).await.unwrap_or_default();
                lists.insert(*flow, list);
            }
        }
        let before = self.candidates.len();
        self.candidates
            .retain(|node, _| waiting.iter().any(|(id, _, _)| id == node));
        let mut replaced = false;
        for (node, device, flow) in waiting {
            let Some(list) = lists.get(&flow) else {
                continue;
            };
            // 端点还在，只是打不开：不是换了端口，不做匹配
            if list.iter().any(|candidate| candidate.endpoint_id == device.endpoint.0) {
                self.candidates.remove(&node);
                continue;
            }
            let workspace = &self.config.workspace;
            let role = match flow {
                AudioFlow::Capture => NodeRole::Input,
                AudioFlow::Render => NodeRole::Output,
            };
            // 同方向上已被其他节点使用的端点不能再用
            let in_use = |endpoint: &str| {
                workspace.nodes().iter().any(|other| {
                    other.id != node
                        && other.kind.role() == role
                        && other.kind.device().is_some_and(|own| own.endpoint.0 == endpoint)
                })
            };
            let matches: Vec<ExternalDevice> = list
                .iter()
                .filter(|candidate| candidate.name == device.name && !in_use(&candidate.endpoint_id))
                .cloned()
                .collect();
            match matches.as_slice() {
                [] => {
                    self.candidates.remove(&node);
                }
                [only] => {
                    let replacement = DeviceRef {
                        endpoint: EndpointId(only.endpoint_id.clone()),
                        name: only.name.clone(),
                    };
                    match self.config.workspace.replace_device(node, replacement) {
                        Ok(()) => {
                            info!(node = node.0, device = %only.name, old = %device.endpoint.0, new = %only.endpoint_id,
                                "the device came back with a new endpoint ID, switched to it");
                            self.candidates.remove(&node);
                            let name = self
                                .config
                                .workspace
                                .node(node)
                                .map(|own| own.name.as_str().to_owned())
                                .unwrap_or_default();
                            self.notify(NoticeKind::DeviceReplaced {
                                node: name,
                                device: only.name.clone(),
                            });
                            replaced = true;
                        }
                        Err(error) => warn!(node = node.0, %error, "cannot switch to the matching device"),
                    }
                }
                _ => {
                    info!(node = node.0, device = %device.name, count = matches.len(), "several devices share the name, waiting for the user");
                    self.candidates.insert(node, matches);
                }
            }
        }
        if replaced {
            self.save();
            self.sync_engine();
        }
        if replaced || before != self.candidates.len() {
            self.schedule_emit();
        }
    }

    // 跨 await 只能借用 &mut self：Controller 是 Send 但不是 Sync（界面推送回调不是 Sync）
    async fn apps(&mut self) -> Result<AppsView, CommandError> {
        let (Some(policy), Some(probe)) = (&self.policy, &self.probe) else {
            return Err(CommandError::Unavailable);
        };
        let sessions = policy.sessions().await.map_err(failed)?;
        let slots = probe.snapshot().await.map_err(failed)?;
        let outputs = probe.external_devices(AudioFlow::Render).await.map_err(failed)?;
        let all_render = policy.endpoints(AudioFlow::Render).await.map_err(failed)?;
        Ok(apps::build(
            sessions,
            &slots,
            outputs,
            &all_render,
            &self.config.workspace,
            &self.config.preferences.app_landings,
        ))
    }

    /// 指派落点并记录（选型报告 5.5 节：Aemeath 记录自己做过的指派，以便日后撤销）。
    async fn set_landing(
        &mut self,
        process_id: u32,
        executable: Option<String>,
        endpoint_id: Option<String>,
    ) -> Result<(), CommandError> {
        let Some(policy) = &self.policy else {
            return Err(CommandError::Unavailable);
        };
        policy
            .set_landing(process_id, endpoint_id.clone())
            .await
            .map_err(failed)?;
        if let Some(executable) = executable {
            let landings = &mut self.config.preferences.app_landings;
            match endpoint_id {
                Some(endpoint) => {
                    landings.insert(executable, endpoint);
                }
                None => {
                    landings.remove(&executable);
                }
            }
            self.save();
        }
        Ok(())
    }

    async fn default_devices(&mut self) -> Result<DefaultDevicesView, CommandError> {
        let Some(policy) = &self.policy else {
            return Err(CommandError::Unavailable);
        };
        let render = policy.endpoints(AudioFlow::Render).await.map_err(failed)?;
        let capture = policy.endpoints(AudioFlow::Capture).await.map_err(failed)?;
        Ok(DefaultDevicesView::new(render, capture, &self.config.workspace))
    }

    fn notify(&mut self, kind: NoticeKind) {
        self.notices.push_back(NoticeView {
            id: self.next_notice,
            kind,
        });
        self.next_notice += 1;
        while self.notices.len() > NOTICE_LIMIT {
            self.notices.pop_front();
        }
    }

    fn schedule_emit(&mut self) {
        self.emit_at.get_or_insert_with(|| Instant::now() + EMIT_COALESCE);
    }

    fn verdicts(&self, node: NodeId, from_output: bool) -> Vec<RouteVerdict> {
        let workspace = &self.config.workspace;
        workspace
            .nodes()
            .iter()
            .map(|other| {
                let (from, to) = if from_output {
                    (node, other.id)
                } else {
                    (other.id, node)
                };
                let rejection = match workspace.check_route(from, to) {
                    Ok(()) => None,
                    Err(EditError::Route(rejection)) => Some(rejection.into()),
                    // 节点已不存在：当作不能连
                    Err(_) => Some(crate::view::RejectionView::SelfRoute),
                };
                RouteVerdict {
                    node: other.id.0,
                    rejection,
                }
            })
            .collect()
    }

    fn matrix_verdicts(&self) -> Vec<PairVerdict> {
        let workspace = &self.config.workspace;
        let nodes = workspace.nodes();
        let senders = nodes.iter().filter(|node| node.kind.can_send());
        senders
            .flat_map(|from| {
                nodes.iter().filter(|to| to.kind.can_receive()).filter_map(move |to| {
                    match workspace.check_route(from.id, to.id) {
                        Err(EditError::Route(rejection)) if rejection != RouteRejection::Duplicate => {
                            Some(PairVerdict {
                                from: from.id.0,
                                to: to.id.0,
                                rejection: rejection.into(),
                            })
                        }
                        _ => None,
                    }
                })
            })
            .collect()
    }

    fn view(&self) -> ViewState {
        let workspace = &self.config.workspace;
        let engine_up = self.engine_state == EngineState::Running;
        let nodes = workspace
            .nodes()
            .iter()
            .map(|node| self.node_view(node, engine_up))
            .collect();
        let slots = all_slots()
            .into_iter()
            .map(|slot| SlotView::new(slot, self.slot_status.get(&slot), workspace.slot_owner(slot)))
            .collect();
        ViewState {
            engine: EngineView {
                state: self.engine_state,
                sample_rate: workspace.engine.sample_rate.hz(),
            },
            registration: self.registration.as_ref().map(|progress| RegistrationView {
                completed: progress.completed(),
                total: progress.total(),
                failing: progress.failing().into_iter().map(SlotRef::from).collect(),
            }),
            can_undo: !self.undo.is_empty(),
            usbip_version: self.usbip_version.clone(),
            language: self.config.preferences.language.clone(),
            nodes,
            routes: workspace.routes().iter().map(RouteView::from).collect(),
            console: workspace.console().iter().map(|id| id.0).collect(),
            slots,
            notices: self.notices.iter().cloned().collect(),
        }
    }

    fn node_view(&self, node: &Node, engine_up: bool) -> NodeView {
        let status = match &node.kind {
            NodeKind::ExternalInput { .. } | NodeKind::ExternalOutput { .. } if engine_up => {
                NodeStatus::of_endpoint(self.endpoint_status.get(&node.id))
            }
            NodeKind::VirtualInput { slot } | NodeKind::VirtualOutput { slot } if engine_up => {
                NodeStatus::of_slot(self.slot_status.get(slot).map(|status| status.health))
            }
            NodeKind::Bus => NodeStatus::Ok,
            _ => NodeStatus::Pending,
        };
        NodeView {
            id: node.id.0,
            name: node.name.as_str().to_owned(),
            kind: kind_view(&node.kind),
            slot: node.kind.slot().map(SlotRef::from),
            device: node.kind.device().map(|device| DeviceView {
                endpoint_id: device.endpoint.0.clone(),
                name: device.name.clone(),
            }),
            x: node.position.x,
            y: node.position.y,
            gain_db: node.strip.gain.db(),
            muted: node.strip.muted,
            gate_db: node.strip.gate.map(GateThreshold::db),
            io: node.device_settings().map(DeviceSettingsView::from),
            format: match self.endpoint_status.get(&node.id) {
                Some(EndpointStatus::Online { format }) if engine_up => Some(FormatView::from(*format)),
                _ => None,
            },
            candidates: self
                .candidates
                .get(&node.id)
                .map(|devices| {
                    devices
                        .iter()
                        .map(|device| DeviceView {
                            endpoint_id: device.endpoint_id.clone(),
                            name: device.name.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            status,
        }
    }
}

fn failed(error: impl std::fmt::Display) -> CommandError {
    CommandError::Failed {
        message: error.to_string(),
    }
}

fn strip_of(workspace: &Workspace, node: NodeId) -> Result<Strip, CommandError> {
    workspace
        .node(node)
        .map(|node| node.strip)
        .ok_or(CommandError::NotFound)
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}
