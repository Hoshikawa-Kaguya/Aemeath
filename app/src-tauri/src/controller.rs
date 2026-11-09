//! 控制面（ADR-0003）：工作台、配置文件、与引擎的 IPC、槽位的挂载维护与首次注册。
//!
//! 线程模型：一个 actor 任务独占全部状态，界面的命令和引擎、槽位的事件都经通道送进来依次处理，不加锁。
//! 每次编辑后：保存配置、把编译出的路由拓扑交给 IPC 客户端（拓扑没变时不下发）、更新被占用的槽位。
//!
//! 首次启动（配置里还没有"已注册"）：先下发注册用拓扑、占用全部 16 个槽位，等全部完成后再换回工作台
//! （ADR-0009）。工作台一开始是空的；工作台为空时界面提供预设（GLOSSARY：预设），节点名由界面按当前语言提供。

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;
use std::time::{Duration, SystemTime};

use aemeath_control::audio_policy::{AppSession, AudioPolicy};
use aemeath_control::engine_client::{ClientConfig, ClientEvent, EngineClient, EngineProber, Timing};
use aemeath_control::hotkeys::{HotkeyEvent, HotkeyService};
use aemeath_control::slots::endpoints::{EndpointProbe, ExternalDevice};
use aemeath_control::slots::registration::{RegistrationProgress, all_slots, registration_topology};
use aemeath_control::slots::{MaintainerConfig, MaintainerTiming, SlotMaintainer, SlotStatus};
use aemeath_control::usbip_cli::UsbipCli;
use aemeath_core::duplicates::{DuplicateRoute, duplicate_routes};
use aemeath_core::hotkeys::{self, HotkeyBinding, HotkeyError, HotkeyOverlay, HotkeyTarget};
use aemeath_core::model::{DeviceRef, LabelPosition, Node, NodeKind, NodeName, Position};
use aemeath_core::preferences::{LandingRecord, LanguagePreference, WindowPlacement};
use aemeath_core::store::{Config, ConfigStore, Loaded};
use aemeath_core::workspace::{EditError, NodeSpec, Workspace};
use aemeath_protocol::ids::{EndpointId, NodeId, ProgramPath, SlotId, SlotNumber, TopologyRevision};
use aemeath_protocol::message::{EndpointDirection, EndpointStatus, OfflineReason, RecorderState};
use aemeath_protocol::topology::{
    DeviceSettings, NodeRole, RecorderSettings, Resampler, RouteRejection, RouteTopology, Strip,
};
use aemeath_protocol::units::{GainDb, GateThreshold, SampleRate};
use aemeath_runtime::config::RuntimeConfig;
use aemeath_sys::audio_endpoints::AudioFlow;
use aemeath_sys::policy_config::DefaultRole;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, MissedTickBehavior};
use tracing::{error, info, warn};

use crate::apps::{self, AppsView};
use crate::default_devices::DefaultDevicesView;
use crate::device_settings::{CapabilitiesView, DeviceSettingsView, ResamplerView};
use crate::hotkey_views::{HotkeyBindingView, HotkeyRowView, HotkeyTargetView};
use crate::recording::{RecorderStateView, RecorderView, RecordingView, prepare_recording_path, recording_folder};
use crate::view::{
    DeviceView, DuplicateView, EngineState, EngineView, FormatView, NodeStatus, NodeView, NoticeKind, NoticeView,
    PairVerdict, ProgramView, RegistrationView, RouteVerdict, RouteView, SlotRef, SlotView, ViewState, kind_view,
};

/// 撤销最多保留的步数
const UNDO_LIMIT: usize = 50;
/// 端点变化后等这么久再做后备匹配，让成批的变化通知都到齐
const REMATCH_SETTLE: Duration = Duration::from_millis(300);
/// 事件密集到达时（例如注册时 16 个槽位同时变化），合并成一次推送
const EMIT_COALESCE: Duration = Duration::from_millis(50);
/// 检查系统是否刚从睡眠唤醒的间隔
const RESUME_PROBE_INTERVAL: Duration = Duration::from_secs(2);
/// 读取音频会话的间隔：发现新会话（纠正落点）、刷新重复路由的判断
const SESSION_POLL_INTERVAL: Duration = Duration::from_secs(2);
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
    #[error("this program already has an app source")]
    ProgramInUse,
    #[error("the program path is not valid")]
    InvalidProgram,
    #[error("a toggle hotkey on the keyboard needs a modifier")]
    HotkeyModifierRequired,
    #[error("this action can only be a toggle")]
    HotkeyToggleOnly,
    #[error("this key is already bound")]
    HotkeyConflict { target: HotkeyTargetView },
    #[error("the node or route no longer exists")]
    NotFound,
    #[error("{message}")]
    Failed { message: String },
}

impl From<HotkeyError> for CommandError {
    fn from(error: HotkeyError) -> Self {
        match error {
            HotkeyError::UnknownTarget => Self::NotFound,
            HotkeyError::ModifierRequired => Self::HotkeyModifierRequired,
            HotkeyError::ToggleOnly => Self::HotkeyToggleOnly,
            HotkeyError::Conflict(target) => Self::HotkeyConflict { target: target.into() },
        }
    }
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
            EditError::ProgramInUse { .. } => Self::ProgramInUse,
            EditError::Route(rejection) => Self::RouteRejected {
                rejection: rejection.into(),
            },
            EditError::InvalidClockMaster
            | EditError::GateNotAllowed(_)
            | EditError::NotAnExternalNode(_)
            | EditError::NotAnAppSource(_)
            | EditError::NotARecorder(_)
            | EditError::InvalidRecorderSettings => Self::Failed {
                message: error.to_string(),
            },
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
    AppSource {
        program: String,
    },
    Recorder,
    #[serde(rename_all = "camelCase")]
    ExternalInsert {
        endpoint_id: String,
        device_name: String,
    },
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
    /// 应用源改绑另一个程序（节点设置里"重新选择程序"）
    ReplaceProgram {
        node: NodeId,
        program: String,
    },
    SetAppResampler {
        node: NodeId,
        resampler: Option<Resampler>,
    },
    SetRecorderSettings {
        node: NodeId,
        settings: RecorderSettings,
    },
    SetHotkey {
        target: HotkeyTarget,
        binding: Option<HotkeyBinding>,
    },
    SetPanicMute(bool),
    SetRecordingFolder(String),
    SetRecordingDefaults(RecorderSettings),
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
    SetLastPage(String),
    SetWindowPlacement(WindowPlacement),
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
                | Self::ReplaceProgram { .. }
                | Self::SetAppResampler { .. }
                | Self::SetRecorderSettings { .. }
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
    SetHotkeysPaused(bool),
    LandingCount {
        node: NodeId,
        reply: oneshot::Sender<usize>,
    },
    Recording {
        node: NodeId,
        start: bool,
        reply: oneshot::Sender<Result<(), CommandError>>,
    },
    Apps(oneshot::Sender<Result<AppsView, CommandError>>),
    /// 执行指派用的句柄（系统调用可能很慢，在 actor 之外等待，不阻塞其他请求）
    LandingPolicy(oneshot::Sender<Option<AudioPolicy>>),
    RecordLanding {
        executable: String,
        endpoint_id: Option<String>,
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
    pub fn start(
        config: RuntimeConfig,
        toggle_window: Box<dyn Fn() + Send>,
        emit: impl Fn(&ViewState) + Send + 'static,
    ) -> Self {
        let (requests, receiver) = mpsc::unbounded_channel();
        tauri::async_runtime::spawn(async move {
            match Controller::new(config, toggle_window, Box::new(emit)).await {
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
        let policy = self
            .ask(Request::LandingPolicy)
            .await?
            .ok_or(CommandError::Unavailable)?;
        policy
            .set_landing(process_id, endpoint_id.clone())
            .await
            .map_err(failed)?;
        if let Some(executable) = executable {
            self.requests
                .send(Request::RecordLanding {
                    executable,
                    endpoint_id,
                })
                .map_err(|_| CommandError::Unavailable)?;
        }
        Ok(())
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

    pub async fn start_recording(&self, node: NodeId) -> Result<(), CommandError> {
        self.ask(|reply| Request::Recording {
            node,
            start: true,
            reply,
        })
        .await?
    }

    pub async fn stop_recording(&self, node: NodeId) -> Result<(), CommandError> {
        self.ask(|reply| Request::Recording {
            node,
            start: false,
            reply,
        })
        .await?
    }

    /// 界面录入新按键期间暂停全部热键。
    pub fn set_hotkeys_paused(&self, paused: bool) {
        let _ = self.requests.send(Request::SetHotkeysPaused(paused));
    }

    /// 落在这个节点上的应用数（Aemeath 做过的指派），删除节点前提示用。
    pub async fn landing_count(&self, node: NodeId) -> Result<usize, CommandError> {
        self.ask(|reply| Request::LandingCount { node, reply }).await
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
    /// 用户在应用页的指派走单独的线程：策略线程按顺序处理请求，指派慢时不能拖住会话轮询
    landing_policy: Option<AudioPolicy>,
    usbip_version: Option<String>,
    engine_state: EngineState,
    endpoint_status: HashMap<NodeId, EndpointStatus>,
    recorder_status: HashMap<NodeId, RecorderState>,
    slot_status: BTreeMap<SlotId, SlotStatus>,
    registration: Option<RegistrationProgress>,
    last_topology: Option<RouteTopology>,
    /// 触发一次按名称的后备匹配（端点增减时、节点报告"等待设备"时）
    rematch_trigger: mpsc::UnboundedSender<()>,
    rematch_events: mpsc::UnboundedReceiver<()>,
    rematch_at: Option<Instant>,
    /// 找到多个同名候选、等用户选择的节点
    candidates: HashMap<NodeId, Vec<ExternalDevice>>,
    /// 最近一次轮询到的音频会话
    sessions: Vec<AppSession>,
    /// 已经处理过的会话（进程 ID）：只在会话出现时纠正一次落点（M3 计划 L1）
    known_sessions: HashSet<u32>,
    /// 槽位当前的端点 ID（判断重复路由时把会话的落点对应到节点）
    slot_endpoints: BTreeMap<SlotId, String>,
    hotkeys: Option<HotkeyService>,
    hotkey_events: mpsc::UnboundedReceiver<HotkeyEvent>,
    /// `RegisterHotKey` 失败的绑定
    occupied_hotkeys: Vec<HotkeyTarget>,
    /// 已注册的绑定，内容不变时不重新注册
    registered_hotkeys: Vec<(HotkeyTarget, HotkeyBinding)>,
    overlay: HotkeyOverlay,
    toggle_window: Box<dyn Fn() + Send>,
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
    async fn new(
        runtime: RuntimeConfig,
        toggle_window: Box<dyn Fn() + Send>,
        emit: Box<dyn Fn(&ViewState) + Send>,
    ) -> Result<Self, StartError> {
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
        let (hotkey_tx, hotkey_events) = mpsc::unbounded_channel();
        let hotkeys = HotkeyService::start(hotkey_tx)
            .inspect_err(|error| warn!(%error, "the hotkey service cannot start"))
            .ok();
        let start_policy = || {
            AudioPolicy::start()
                .inspect_err(|error| warn!(%error, "the audio policy service cannot start"))
                .ok()
        };
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
            policy: start_policy(),
            landing_policy: start_policy(),
            usbip_version,
            engine_state: EngineState::Connecting,
            endpoint_status: HashMap::new(),
            recorder_status: HashMap::new(),
            slot_status: BTreeMap::new(),
            registration,
            last_topology: None,
            rematch_trigger,
            rematch_events,
            rematch_at: None,
            candidates: HashMap::new(),
            sessions: Vec::new(),
            known_sessions: HashSet::new(),
            slot_endpoints: BTreeMap::new(),
            hotkeys,
            hotkey_events,
            occupied_hotkeys: Vec::new(),
            registered_hotkeys: Vec::new(),
            overlay: HotkeyOverlay::default(),
            toggle_window,
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
        let mut session_poll = tokio::time::interval(SESSION_POLL_INTERVAL);
        // 默认的 Burst 会在 actor 忙过之后连续补跑错过的轮询，让界面的请求继续排队
        session_poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            let emit_at = self.emit_at;
            let rematch_at = self.rematch_at;
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
                    // 端点变化往往成批到达（一台设备有多个端点）：合并成一次匹配。
                    // 用定时分支等待，不能在这里 sleep，否则这段时间界面的请求全都要排队
                    self.rematch_at.get_or_insert_with(|| Instant::now() + REMATCH_SETTLE);
                }
                () = sleep_until(rematch_at), if rematch_at.is_some() => {
                    self.rematch_at = None;
                    self.rematch().await;
                }
                _ = session_poll.tick() => self.poll_sessions().await,
                Some(event) = self.hotkey_events.recv() => self.on_hotkey(event),
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
                let released = match &edit {
                    Edit::RemoveNode(node) => self.landing_endpoint(*node).await,
                    _ => None,
                };
                let result = self.apply(edit).map(|()| self.view());
                let succeeded = result.is_ok();
                let _ = reply.send(result);
                if let Some(endpoint) = released.filter(|_| succeeded) {
                    self.release_landings(&endpoint).await;
                }
            }
            Request::Recording { node, start, reply } => {
                let _ = reply.send(if start {
                    self.start_recording(node)
                } else {
                    self.stop_recording(node)
                });
            }
            Request::SetHotkeysPaused(paused) => {
                if let Some(service) = &self.hotkeys {
                    if paused {
                        service.pause();
                    } else {
                        service.resume();
                    }
                }
            }
            Request::LandingCount { node, reply } => {
                let endpoint = self.landing_endpoint(node).await;
                let _ = reply.send(endpoint.map_or(0, |endpoint| self.landings_on(&endpoint).count()));
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
            Request::LandingPolicy(reply) => {
                let _ = reply.send(self.landing_policy.clone());
            }
            Request::RecordLanding {
                executable,
                endpoint_id,
            } => self.record_landing(executable, endpoint_id),
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
                    NewNodeKind::AppSource { program } => NodeSpec::AppSource(parse_program(&program)?),
                    NewNodeKind::Recorder => NodeSpec::Recorder(self.config.preferences.recording_defaults),
                    NewNodeKind::ExternalInsert {
                        endpoint_id,
                        device_name,
                    } => NodeSpec::ExternalInsert {
                        ret: device(endpoint_id, device_name),
                        send: None,
                    },
                };
                let node = workspace.add_node(name, spec, position)?;
                if let Some(settings) = io {
                    workspace.set_device_settings(node, settings)?;
                }
            }
            Edit::ReplaceProgram { node, program } => workspace.replace_program(node, parse_program(&program)?)?,
            Edit::SetAppResampler { node, resampler } => workspace.set_app_resampler(node, resampler)?,
            Edit::SetRecorderSettings { node, settings } => workspace.set_recorder_settings(node, settings)?,
            Edit::SetHotkey { target, binding } => {
                hotkeys::set_hotkey(workspace, &mut self.config.preferences, target, binding)?;
            }
            Edit::SetPanicMute(active) => self.overlay.panic_mute = active,
            Edit::SetRecordingFolder(folder) => self.config.preferences.recording_folder = Some(folder),
            Edit::SetRecordingDefaults(settings) => {
                if !settings.is_valid() {
                    return Err(EditError::InvalidRecorderSettings.into());
                }
                self.config.preferences.recording_defaults = settings;
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
            Edit::SetLastPage(page) => self.config.preferences.last_page = Some(page),
            Edit::SetWindowPlacement(placement) => {
                let window = &mut self.config.preferences.window;
                // 最大化时读到的是整个屏幕，保留上次的还原位置和大小
                *window = Some(match *window {
                    Some(previous) if placement.maximized => WindowPlacement {
                        maximized: true,
                        ..previous
                    },
                    _ => placement,
                });
            }
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
            let mut topology = workspace.compile(TopologyRevision(0));
            self.overlay.apply(&mut topology);
            (topology, workspace.occupied_slots().collect())
        };
        if self.last_topology.as_ref() != Some(&topology) {
            self.client.apply(topology.clone());
            self.last_topology = Some(topology);
        }
        if let Some(maintainer) = &self.maintainer {
            maintainer.set_engine_rate(rate);
            maintainer.set_occupied(occupied);
        }
        self.sync_hotkeys();
    }

    /// 绑定列表变了才重新注册，避免每次编辑都注销再注册全局热键。
    fn sync_hotkeys(&mut self) {
        let bindings = hotkeys::all_hotkeys(&self.config.workspace, &self.config.preferences);
        if bindings == self.registered_hotkeys {
            return;
        }
        // 被删除的对象不再有松开事件，清掉它们的临时叠加
        self.overlay
            .held
            .retain(|target| bindings.iter().any(|(own, _)| own == target));
        if let Some(service) = &self.hotkeys {
            self.occupied_hotkeys = service.set_bindings(bindings.clone());
        }
        self.registered_hotkeys = bindings;
    }

    fn on_hotkey(&mut self, event: HotkeyEvent) {
        match event {
            HotkeyEvent::Fired(target) => self.fire_hotkey(target),
            HotkeyEvent::Pressed(target) => {
                self.overlay.held.insert(target);
                self.sync_engine();
            }
            HotkeyEvent::Released(target) => {
                self.overlay.held.remove(&target);
                self.sync_engine();
            }
        }
        self.schedule_emit();
    }

    /// 切换型热键（M3 计划 H10）：静音和连线开关是正式修改，写进配置，但不进撤销记录。
    fn fire_hotkey(&mut self, target: HotkeyTarget) {
        match target {
            HotkeyTarget::NodeMute(node) => {
                let Some(strip) = self.config.workspace.node(node).map(|own| own.strip) else {
                    return;
                };
                let changed = Strip {
                    muted: !strip.muted,
                    ..strip
                };
                if self.config.workspace.set_strip(node, changed).is_ok() {
                    self.save();
                    self.sync_engine();
                }
            }
            HotkeyTarget::RouteToggle(from, to) => {
                let Some(enabled) = self.config.workspace.route(from, to).map(|route| route.enabled) else {
                    return;
                };
                if self.config.workspace.set_route_enabled(from, to, !enabled).is_ok() {
                    self.save();
                    self.sync_engine();
                }
            }
            HotkeyTarget::NodeRecord(node) => {
                let recording = matches!(self.recorder_status.get(&node), Some(RecorderState::Recording { .. }));
                let result = if recording {
                    self.stop_recording(node)
                } else {
                    self.start_recording(node)
                };
                if let Err(error) = result {
                    warn!(%error, "cannot toggle the recording");
                }
            }
            HotkeyTarget::AllRecording => self.toggle_all_recording(),
            HotkeyTarget::PanicMute => {
                self.overlay.panic_mute = !self.overlay.panic_mute;
                self.sync_engine();
            }
            HotkeyTarget::ToggleWindow => (self.toggle_window)(),
        }
    }

    /// 只要有一个录音节点在录音就全部停止，否则全部开始（M3 计划 H3）。
    fn toggle_all_recording(&mut self) {
        let recorders: Vec<NodeId> = self
            .config
            .workspace
            .nodes()
            .iter()
            .filter(|node| matches!(node.kind, NodeKind::Recorder { .. }))
            .map(|node| node.id)
            .collect();
        let any_recording = recorders
            .iter()
            .any(|node| matches!(self.recorder_status.get(node), Some(RecorderState::Recording { .. })));
        for node in recorders {
            let result = if any_recording {
                self.stop_recording(node)
            } else {
                self.start_recording(node)
            };
            if let Err(error) = result {
                warn!(%error, node = node.0, "cannot toggle the recording");
            }
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
                self.recorder_status.clear();
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
            ClientEvent::RecorderStatus { node, status } => {
                if let RecorderState::Failed { reason } = status {
                    let name = self
                        .config
                        .workspace
                        .node(node)
                        .map(|own| own.name.as_str().to_owned())
                        .unwrap_or_default();
                    self.notify(NoticeKind::RecordingFailed {
                        node: name,
                        reason: reason.into(),
                    });
                }
                self.recorder_status.insert(node, status);
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
                    NodeKind::ExternalInput { .. } | NodeKind::ExternalInsert { .. } => AudioFlow::Capture,
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
                        && other.kind.endpoint_role() == Some(role)
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
        let slots = probe.slot_endpoints().await.map_err(failed)?;
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

    /// 记录成功的指派（选型报告 5.5 节：Aemeath 记录自己做过的指派，以便日后撤销）。
    fn record_landing(&mut self, executable: String, endpoint_id: Option<String>) {
        let landings = &mut self.config.preferences.app_landings;
        match endpoint_id {
            Some(endpoint) => {
                landings.insert(executable, LandingRecord::Endpoint(EndpointId(endpoint)));
            }
            None => {
                landings.remove(&executable);
            }
        }
        self.save();
    }

    /// 拼出文件路径、确保文件夹存在，然后让引擎开始录音（M3 计划 R6）。
    fn start_recording(&self, id: NodeId) -> Result<(), CommandError> {
        let node = self.config.workspace.node(id).ok_or(CommandError::NotFound)?;
        let NodeKind::Recorder { settings } = node.kind else {
            return Err(CommandError::NotFound);
        };
        let folder = recording_folder(&self.config.preferences);
        let path = prepare_recording_path(&folder, node.name.as_str(), settings).map_err(failed)?;
        self.client.start_recording(id, path.display().to_string());
        Ok(())
    }

    fn stop_recording(&self, id: NodeId) -> Result<(), CommandError> {
        match self.config.workspace.node(id).map(|node| &node.kind) {
            Some(NodeKind::Recorder { .. }) => {
                self.client.stop_recording(id);
                Ok(())
            }
            _ => Err(CommandError::NotFound),
        }
    }

    /// 节点作为落点时的端点 ID：只有虚拟输入和外部输出会被当作落点。
    async fn landing_endpoint(&mut self, node: NodeId) -> Option<EndpointId> {
        match &self.config.workspace.node(node)?.kind {
            NodeKind::VirtualInput { slot } => {
                let slot = *slot;
                self.probe
                    .as_ref()?
                    .slot_endpoints()
                    .await
                    .ok()?
                    .get(&slot)
                    .map(|endpoint| EndpointId(endpoint.id.clone()))
            }
            NodeKind::ExternalOutput { device } => Some(device.endpoint.clone()),
            _ => None,
        }
    }

    fn landings_on<'a>(&'a self, endpoint: &'a EndpointId) -> impl Iterator<Item = &'a str> + 'a {
        self.config
            .preferences
            .app_landings
            .iter()
            .filter(move |(_, record)| matches!(record, LandingRecord::Endpoint(own) if own == endpoint))
            .map(|(executable, _)| executable.as_str())
    }

    /// 落点节点被删除后（M3 计划 L3）：正有会话的程序立即清除指派，其余的记为"待清除"，
    /// 免得这个槽位以后被别的节点占用时，这些程序意外落进去。
    // shortcut: 撤销删除节点时不恢复这些指派，用户要在应用页重新指派；需要时把记录放进撤销快照
    async fn release_landings(&mut self, endpoint: &EndpointId) {
        let affected: Vec<String> = self.landings_on(endpoint).map(str::to_owned).collect();
        if affected.is_empty() {
            return;
        }
        let Some(policy) = self.policy.clone() else {
            return;
        };
        let sessions = policy.sessions().await.unwrap_or_default();
        for executable in affected {
            let session = sessions
                .iter()
                .find(|session| session_executable(session).as_deref() == Some(executable.as_str()));
            let cleared = match session {
                Some(session) => policy.set_landing(session.process_id, None).await.is_ok(),
                None => false,
            };
            let landings = &mut self.config.preferences.app_landings;
            if cleared {
                landings.remove(&executable);
            } else {
                landings.insert(executable, LandingRecord::PendingClear);
            }
        }
        self.save();
    }

    /// 定时读取音频会话：新出现的程序按记录纠正落点（M3 计划 L1、L3），并刷新判断重复路由所需的数据。
    async fn poll_sessions(&mut self) {
        let Some(policy) = self.policy.clone() else {
            return;
        };
        let Ok(sessions) = policy.sessions().await else {
            return;
        };
        let mut slot_endpoints = BTreeMap::new();
        if let Some(probe) = &self.probe {
            slot_endpoints = probe
                .slot_endpoints()
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|(slot, endpoint)| (slot, endpoint.id))
                .collect();
        }
        let mut changed = slot_endpoints != self.slot_endpoints || sessions != self.sessions;
        // 先更新端点表：纠正落点的提示要靠它找到节点名
        self.slot_endpoints = slot_endpoints;
        for session in &sessions {
            if !self.known_sessions.contains(&session.process_id) {
                changed |= self.on_new_session(&policy, session).await;
            }
        }
        self.known_sessions = sessions.iter().map(|session| session.process_id).collect();
        self.sessions = sessions;
        if changed {
            self.schedule_emit();
        }
    }

    /// 返回是否改动了配置或提示。
    async fn on_new_session(&mut self, policy: &AudioPolicy, session: &AppSession) -> bool {
        let Some(executable) = session_executable(session) else {
            return false;
        };
        match self.config.preferences.app_landings.get(&executable).cloned() {
            Some(LandingRecord::PendingClear) => {
                if policy.set_landing(session.process_id, None).await.is_ok() {
                    self.config.preferences.app_landings.remove(&executable);
                    self.save();
                    return true;
                }
                false
            }
            Some(LandingRecord::Endpoint(endpoint)) => {
                let Ok(current) = policy.landing(session.process_id).await else {
                    return false;
                };
                if current.as_deref() == Some(endpoint.0.as_str()) {
                    return false;
                }
                if let Err(error) = policy.set_landing(session.process_id, Some(endpoint.0.clone())).await {
                    warn!(%error, %executable, "cannot restore the app landing");
                    return false;
                }
                info!(%executable, endpoint = %endpoint.0, ?current, "restored the app landing");
                let landing = self.landing_name(&endpoint);
                self.notify(NoticeKind::LandingRestored {
                    app: program_stem(&executable),
                    landing,
                });
                true
            }
            None => false,
        }
    }

    /// 落点的显示名：工作台上的节点名，不在工作台上时用端点 ID（极少见：节点被删除时记录已经改掉了）。
    fn landing_name(&self, endpoint: &EndpointId) -> String {
        self.config
            .workspace
            .nodes()
            .iter()
            .find(|node| self.is_landing_endpoint(node, &endpoint.0))
            .map_or_else(|| endpoint.0.clone(), |node| node.name.as_str().to_owned())
    }

    fn is_landing_endpoint(&self, node: &Node, endpoint: &str) -> bool {
        match &node.kind {
            NodeKind::VirtualInput { slot } => self.slot_endpoints.get(slot).is_some_and(|own| own == endpoint),
            NodeKind::ExternalOutput { device } => device.endpoint.0 == endpoint,
            _ => false,
        }
    }

    /// 当前的重复路由（M3 计划 L5）：落点按会话当前所在的端点实时判断。
    fn duplicates(&self) -> Vec<DuplicateRoute> {
        let workspace = &self.config.workspace;
        let landings: Vec<(NodeId, NodeId)> = workspace
            .nodes()
            .iter()
            .filter_map(|node| Some((node.id, node.kind.program()?)))
            .flat_map(|(source, program)| {
                self.sessions
                    .iter()
                    .filter(move |session| {
                        session_executable(session)
                            .and_then(|path| ProgramPath::new(&path).ok())
                            .is_some_and(|path| path.same_program(program))
                    })
                    .filter_map(move |session| {
                        let landing = workspace
                            .nodes()
                            .iter()
                            .find(|node| self.is_landing_endpoint(node, &session.endpoint_id))?;
                        Some((source, landing.id))
                    })
            })
            .collect();
        duplicate_routes(workspace, &landings)
    }

    fn duplicate_view(&self, duplicate: &DuplicateRoute) -> DuplicateView {
        let name = |id: NodeId| {
            self.config
                .workspace
                .node(id)
                .map_or_else(String::new, |node| node.name.as_str().to_owned())
        };
        DuplicateView {
            program: name(duplicate.source),
            destination: name(duplicate.destination),
        }
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
        let duplicates = self.duplicates();
        let nodes = workspace
            .nodes()
            .iter()
            .map(|node| {
                let duplicate = duplicates
                    .iter()
                    .find(|duplicate| duplicate.source == node.id)
                    .map(|duplicate| self.duplicate_view(duplicate));
                self.node_view(node, engine_up, duplicate)
            })
            .collect();
        let routes = workspace
            .routes()
            .iter()
            .map(|route| {
                let duplicate = duplicates
                    .iter()
                    .find(|duplicate| duplicate.routes.contains(&(route.from, route.to)))
                    .map(|duplicate| self.duplicate_view(duplicate));
                RouteView::new(route, duplicate)
            })
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
            last_page: self.config.preferences.last_page.clone(),
            duplicates: duplicates
                .iter()
                .map(|duplicate| self.duplicate_view(duplicate))
                .collect(),
            nodes,
            routes,
            console: workspace.console().iter().map(|id| id.0).collect(),
            hotkeys: hotkeys::all_hotkeys(workspace, &self.config.preferences)
                .into_iter()
                .map(|(target, binding)| HotkeyRowView {
                    target: target.into(),
                    binding: binding.into(),
                    occupied: self.occupied_hotkeys.contains(&target),
                })
                .collect(),
            panic_mute: self.overlay.panic_mute,
            recording: RecordingView {
                folder: recording_folder(&self.config.preferences).display().to_string(),
                defaults: self.config.preferences.recording_defaults.into(),
            },
            slots,
            notices: self.notices.iter().cloned().collect(),
        }
    }

    fn node_view(&self, node: &Node, engine_up: bool, duplicate: Option<DuplicateView>) -> NodeView {
        let status = match &node.kind {
            NodeKind::ExternalInput { .. }
            | NodeKind::ExternalOutput { .. }
            | NodeKind::AppSource { .. }
            | NodeKind::ExternalInsert { .. }
                if engine_up =>
            {
                NodeStatus::of_endpoint(self.endpoint_status.get(&node.id))
            }
            NodeKind::VirtualInput { slot } | NodeKind::VirtualOutput { slot } if engine_up => {
                NodeStatus::of_slot(self.slot_status.get(slot).map(|status| status.health))
            }
            NodeKind::Bus | NodeKind::Recorder { .. } => NodeStatus::Ok,
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
            program: node.kind.program().map(|program| ProgramView {
                path: program.as_str().to_owned(),
                file_name: program.file_name().to_owned(),
            }),
            resampler: match node.kind {
                NodeKind::AppSource { resampler, .. } => resampler.map(ResamplerView::from),
                _ => None,
            },
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
            mute_hotkey: node.mute_hotkey.map(HotkeyBindingView::from),
            record_hotkey: node.record_hotkey.map(HotkeyBindingView::from),
            recorder: match node.kind {
                NodeKind::Recorder { settings } => Some(RecorderView {
                    settings: settings.into(),
                    state: self
                        .recorder_status
                        .get(&node.id)
                        .map_or(RecorderStateView::Idle, RecorderStateView::from),
                }),
                _ => None,
            },
            duplicate,
        }
    }
}

fn session_executable(session: &AppSession) -> Option<String> {
    session.executable.as_ref().map(|path| path.display().to_string())
}

fn program_stem(executable: &str) -> String {
    Path::new(executable)
        .file_stem()
        .map_or_else(|| executable.to_owned(), |stem| stem.to_string_lossy().into_owned())
}

fn failed(error: impl std::fmt::Display) -> CommandError {
    CommandError::Failed {
        message: error.to_string(),
    }
}

fn parse_program(path: &str) -> Result<ProgramPath, CommandError> {
    ProgramPath::new(path).map_err(|_| CommandError::InvalidProgram)
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
