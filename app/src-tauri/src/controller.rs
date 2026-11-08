//! 控制面（ADR-0003）：工作台、配置文件、与引擎的 IPC、槽位的挂载维护与首次注册。
//!
//! 线程模型：一个 actor 任务独占全部状态，界面的命令和引擎、槽位的事件都经通道送进来依次处理，不加锁。
//! 每次编辑后：保存配置、把编译出的路由拓扑交给 IPC 客户端（拓扑没变时不下发）、更新被占用的槽位。
//!
//! 首次启动（配置里还没有"已注册"）：先下发注册用拓扑、占用全部 16 个槽位，等全部完成后再换回工作台
//! （ADR-0009）。默认输入的节点名由界面按当前语言提供（`initialize`）。

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::{Duration, SystemTime};

use aemeath_control::engine_client::{ClientConfig, ClientEvent, EngineClient, Timing};
use aemeath_control::slots::endpoints::{EndpointProbe, ExternalDevice};
use aemeath_control::slots::registration::{RegistrationProgress, all_slots, registration_topology};
use aemeath_control::slots::{MaintainerConfig, MaintainerTiming, SlotMaintainer, SlotStatus};
use aemeath_control::usbip_cli::UsbipCli;
use aemeath_core::model::{DeviceRef, LabelPosition, Node, NodeKind, NodeName, Position};
use aemeath_core::preferences::LanguagePreference;
use aemeath_core::store::{Config, ConfigStore, Loaded};
use aemeath_core::workspace::{EditError, NodeSpec, Workspace};
use aemeath_protocol::ids::{EndpointId, NodeId, SlotId, SlotNumber, TopologyRevision};
use aemeath_protocol::message::EndpointStatus;
use aemeath_protocol::topology::RouteTopology;
use aemeath_protocol::units::GainDb;
use aemeath_runtime::config::RuntimeConfig;
use aemeath_sys::audio_endpoints::AudioFlow;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tracing::{error, info, warn};

use crate::view::{
    DeviceView, EngineState, EngineView, NodeStatus, NodeView, RegistrationView, RouteVerdict, RouteView, SlotRef,
    SlotView, ViewState, kind_view,
};

/// 撤销最多保留的步数
const UNDO_LIMIT: usize = 50;
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
            EditError::InvalidClockMaster => Self::Failed {
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
    /// 首次启动：创建默认输入（占用 In 1）。名称由界面按当前语言提供
    Initialize {
        default_input_name: String,
    },
    AddNode {
        kind: NewNodeKind,
        name: String,
        position: Position,
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
    SetRouteLabel {
        from: NodeId,
        to: NodeId,
        position: f32,
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
                | Self::RemoveNode(_)
                | Self::RenameNode { .. }
                | Self::Connect { .. }
                | Self::Disconnect { .. }
                | Self::SetRouteEnabled { .. }
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
    first_run: bool,
    undo: Vec<Workspace>,
    client: EngineClient,
    client_events: mpsc::UnboundedReceiver<ClientEvent>,
    maintainer: Option<SlotMaintainer>,
    slot_events: mpsc::UnboundedReceiver<SlotStatus>,
    probe: Option<EndpointProbe>,
    usbip_version: Option<String>,
    engine_state: EngineState,
    endpoint_status: HashMap<NodeId, EndpointStatus>,
    slot_status: BTreeMap<SlotId, SlotStatus>,
    registration: Option<RegistrationProgress>,
    last_topology: Option<RouteTopology>,
    emit: Box<dyn Fn(&ViewState) + Send>,
    emit_at: Option<Instant>,
}

impl Controller {
    async fn new(runtime: RuntimeConfig, emit: Box<dyn Fn(&ViewState) + Send>) -> Result<Self, StartError> {
        let store = ConfigStore::new(&runtime.data_dir);
        let (config, read_only, first_run) = match store.load() {
            Ok(Loaded::Existing(config)) => (config, false, false),
            Ok(Loaded::Missing) => {
                info!("no configuration yet, starting with an empty workspace");
                (Config::default(), false, true)
            }
            Ok(Loaded::Recovered { config, backup, reason }) => {
                warn!(%reason, backup = %backup.display(), "the configuration was invalid and has been backed up");
                (config, false, true)
            }
            Err(error) => {
                error!(%error, "cannot use the configuration file, changes will not be saved");
                (Config::default(), true, false)
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
        let probe =
            EndpointProbe::start(identities, || {}).map_err(|error| StartError::Endpoints(error.to_string()))?;

        let registration = (!config.preferences.slots_registered).then(|| {
            info!("the slots are not registered yet, registering all 16");
            RegistrationProgress::new(rate)
        });
        let mut controller = Self {
            config,
            store,
            read_only,
            first_run,
            undo: Vec::new(),
            client,
            client_events,
            maintainer,
            slot_events,
            probe: Some(probe),
            usbip_version,
            engine_state: EngineState::Connecting,
            endpoint_status: HashMap::new(),
            slot_status: BTreeMap::new(),
            registration,
            last_topology: None,
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
            Edit::Initialize { default_input_name } => {
                if self.first_run && workspace.nodes().is_empty() {
                    let name = NodeName::new(&default_input_name).map_err(|_| CommandError::InvalidName)?;
                    let slot = SlotNumber::new(SlotNumber::FIRST).ok();
                    workspace.add_node(name, NodeSpec::VirtualInput(slot), Position { x: 40.0, y: 40.0 })?;
                }
                self.first_run = false;
            }
            Edit::AddNode { kind, name, position } => {
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
                };
                workspace.add_node(name, spec, position)?;
            }
            Edit::RemoveNode(node) => {
                workspace.remove_node(node)?;
            }
            Edit::RenameNode { node, name } => {
                let name = NodeName::new(&name).map_err(|_| CommandError::InvalidName)?;
                workspace.rename_node(node, name)?;
            }
            Edit::MoveNode { node, position } => workspace.move_node(node, position)?,
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
            first_run: self.first_run,
            can_undo: !self.undo.is_empty(),
            usbip_version: self.usbip_version.clone(),
            language: self.config.preferences.language.clone(),
            nodes,
            routes: workspace.routes().iter().map(RouteView::from).collect(),
            slots,
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
            status,
        }
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}
