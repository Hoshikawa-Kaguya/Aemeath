//! 工作台：节点、连线与引擎设置，以及对它们的全部编辑操作。
//!
//! 字段只能通过方法修改，以保证规则始终成立：
//! - 每个槽位最多被一个节点占用，方向与节点类型一致；新建虚拟节点时自动分配编号最小的空闲槽位。
//! - 同一个外部设备在同一方向上只对应一个节点。
//! - 连线规则与路由拓扑校验相同（[`topology::check_route`]）；拒绝时给出原因，供工作台拖线时提示。
//! - 节点 ID 只增不减、不复用（引擎换拓扑时按 ID 接过旧状态，复用 ID 会让新节点继承旧节点的增益）。

use std::collections::BTreeMap;

use aemeath_protocol::ids::{NodeId, ProgramPath, SlotDirection, SlotId, SlotNumber, TopologyRevision};
use aemeath_protocol::topology::{
    self, DeviceSettings, NodeRole, RecorderSettings, Resampler, RouteCheckError, RouteRejection, SlotFormatChoice,
    SlotNodeSettings, Strip,
};
use aemeath_protocol::units::GainDb;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::hotkeys::HotkeyBinding;
use crate::model::{DeviceRef, EngineSettings, LabelPosition, Node, NodeKind, NodeName, Position, Route};

/// 第一个节点的 ID
const FIRST_NODE_ID: u32 = 1;

/// 用户编辑的工作台：节点、连线、控制台和引擎设置。所有修改都经这里的方法，规则见模块说明。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    pub engine: EngineSettings,
    nodes: Vec<Node>,
    routes: Vec<Route>,
    /// 下一个新节点的 ID（只增不减）
    next_node_id: u32,
    /// 被固定到控制台的节点，按控制台上从左到右的顺序（GLOSSARY：固定）
    #[serde(default)]
    console: Vec<NodeId>,
}

/// 新建节点时的类型；虚拟节点可以指定槽位编号，不指定时自动分配。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeSpec {
    ExternalInput(DeviceRef),
    ExternalOutput(DeviceRef),
    VirtualInput(Option<SlotNumber>),
    VirtualOutput(Option<SlotNumber>),
    Bus,
    AppSource(ProgramPath),
    /// 外部插入：取回端设备；送出端槽位不指定时自动分配
    ExternalInsert {
        ret: DeviceRef,
        send: Option<SlotNumber>,
    },
    Recorder(RecorderSettings),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EditError {
    #[error("node {0} does not exist")]
    UnknownNode(NodeId),
    #[error("there is no route from {from} to {to}")]
    UnknownRoute { from: NodeId, to: NodeId },
    #[error("all {0:?} slots are occupied")]
    NoFreeSlot(SlotDirection),
    #[error("slot {slot} is occupied by {by}")]
    SlotOccupied { slot: SlotId, by: NodeId },
    #[error("node {0} does not occupy a slot")]
    NotAVirtualNode(NodeId),
    #[error("this device is already used by {by} in the same direction")]
    DeviceInUse { by: NodeId },
    #[error("this program already has an app source: {by}")]
    ProgramInUse { by: NodeId },
    #[error("node {0} is not an app source")]
    NotAnAppSource(NodeId),
    #[error("node {0} does not occupy a slot")]
    NotASlotNode(NodeId),
    #[error("node {0} is not a recorder")]
    NotARecorder(NodeId),
    #[error("FLAC cannot store floating-point samples")]
    InvalidRecorderSettings,
    #[error("the clock master must be an external device node")]
    InvalidClockMaster,
    #[error("node {0} is not an input node and cannot have a gate")]
    GateNotAllowed(NodeId),
    #[error("node {0} is not an external device node")]
    NotAnExternalNode(NodeId),
    #[error("cannot route: {0}")]
    Route(#[from] RouteRejection),
    /// 编辑后的工作台过不了读配置时的校验，保存了下次启动会被当成损坏
    #[error("the workspace is not valid: {0}")]
    Invalid(String),
}

impl From<RouteCheckError> for EditError {
    fn from(error: RouteCheckError) -> Self {
        match error {
            RouteCheckError::UnknownNode(id) => Self::UnknownNode(id),
            RouteCheckError::Rejected(rejection) => Self::Route(rejection),
        }
    }
}

impl Workspace {
    pub fn new(engine: EngineSettings) -> Self {
        Self {
            engine,
            nodes: Vec::new(),
            routes: Vec::new(),
            next_node_id: FIRST_NODE_ID,
            console: Vec::new(),
        }
    }

    /// 控制台上的节点，从左到右。
    pub fn console(&self) -> &[NodeId] {
        &self.console
    }

    /// 节点是否固定在控制台上。
    pub fn is_pinned(&self, id: NodeId) -> bool {
        self.console.contains(&id)
    }

    /// 固定到控制台的最右端；已固定时不变。
    pub fn pin(&mut self, id: NodeId) -> Result<(), EditError> {
        self.index_of(id)?;
        if !self.console.contains(&id) {
            self.console.push(id);
        }
        Ok(())
    }

    /// 从控制台上取下；没固定时不变。
    pub fn unpin(&mut self, id: NodeId) {
        self.console.retain(|pinned| *pinned != id);
    }

    /// 在控制台上把节点移到第 `index` 个位置（超出范围时放到最右端）。
    pub fn move_pin(&mut self, id: NodeId, index: usize) -> Result<(), EditError> {
        let from = self
            .console
            .iter()
            .position(|pinned| *pinned == id)
            .ok_or(EditError::UnknownNode(id))?;
        self.console.remove(from);
        self.console.insert(index.min(self.console.len()), id);
        Ok(())
    }

    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    pub fn routes(&self) -> &[Route] {
        &self.routes
    }

    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.iter().find(|node| node.id == id)
    }

    pub fn route(&self, from: NodeId, to: NodeId) -> Option<&Route> {
        self.routes.iter().find(|route| route.from == from && route.to == to)
    }

    /// 槽位的占用者（设备页显示）。
    pub fn slot_owner(&self, slot: SlotId) -> Option<&Node> {
        self.nodes.iter().find(|node| node.kind.slot() == Some(slot))
    }

    /// 编号最小的空闲槽位。
    pub fn free_slot(&self, direction: SlotDirection) -> Option<SlotId> {
        SlotId::all(direction).find(|slot| self.slot_owner(*slot).is_none())
    }

    /// 被占用的全部槽位。
    pub fn occupied_slots(&self) -> impl Iterator<Item = SlotId> + '_ {
        self.nodes.iter().filter_map(|node| node.kind.slot())
    }

    /// 添加节点并返回它的 ID。虚拟节点没指定槽位时自动分配编号最小的空闲槽位；
    /// 设备或程序已被其他节点占用、槽位不可用时报错，工作台不变。
    pub fn add_node(&mut self, name: NodeName, spec: NodeSpec, position: Position) -> Result<NodeId, EditError> {
        let kind = match spec {
            NodeSpec::ExternalInput(device) => {
                self.check_device(&device, NodeRole::Input)?;
                NodeKind::ExternalInput { device }
            }
            NodeSpec::ExternalOutput(device) => {
                self.check_device(&device, NodeRole::Output)?;
                NodeKind::ExternalOutput { device }
            }
            NodeSpec::VirtualInput(number) => NodeKind::VirtualInput {
                slot: self.claim_slot(SlotDirection::In, number)?,
            },
            NodeSpec::VirtualOutput(number) => NodeKind::VirtualOutput {
                slot: self.claim_slot(SlotDirection::Out, number)?,
            },
            NodeSpec::Bus => NodeKind::Bus,
            NodeSpec::ExternalInsert { ret, send } => {
                self.check_device(&ret, NodeRole::Input)?;
                NodeKind::ExternalInsert {
                    send: self.claim_slot(SlotDirection::Out, send)?,
                    ret,
                }
            }
            NodeSpec::Recorder(settings) => {
                if !settings.is_valid() {
                    return Err(EditError::InvalidRecorderSettings);
                }
                NodeKind::Recorder { settings }
            }
            NodeSpec::AppSource(program) => {
                self.check_program(&program, None)?;
                NodeKind::AppSource {
                    program,
                    resampler: None,
                }
            }
        };
        let id = NodeId(self.next_node_id);
        self.next_node_id += 1;
        let io = kind.is_external().then(DeviceSettings::default);
        let slot_settings = kind.slot().map(|_| SlotNodeSettings::default());
        self.nodes.push(Node {
            id,
            name,
            kind,
            strip: Strip::default(),
            io,
            slot_settings,
            position,
            mute_hotkey: None,
            record_hotkey: None,
        });
        Ok(id)
    }

    /// 删除节点，连同与它相连的连线；它是主时钟时清除主时钟设置。它占用的槽位随之空出（拔出）。
    pub fn remove_node(&mut self, id: NodeId) -> Result<Node, EditError> {
        let index = self.index_of(id)?;
        self.routes.retain(|route| route.from != id && route.to != id);
        self.unpin(id);
        if self.engine.clock_master == Some(id) {
            self.engine.clock_master = None;
        }
        Ok(self.nodes.remove(index))
    }

    /// 改名。节点名允许重复，靠代号区分。
    pub fn rename_node(&mut self, id: NodeId, name: NodeName) -> Result<(), EditError> {
        self.node_mut(id)?.name = name;
        Ok(())
    }

    /// 移动节点在工作台上的位置。
    pub fn move_node(&mut self, id: NodeId, position: Position) -> Result<(), EditError> {
        self.node_mut(id)?.position = position;
        Ok(())
    }

    /// 设定通道条。只有输入节点可以设门限（GLOSSARY：门限）。
    pub fn set_strip(&mut self, id: NodeId, strip: Strip) -> Result<(), EditError> {
        let node = self.node_mut(id)?;
        if strip.gate.is_some() && node.kind.role() != NodeRole::Input {
            return Err(EditError::GateNotAllowed(id));
        }
        node.strip = strip;
        Ok(())
    }

    /// 让外部设备节点改用另一个端点（按名称的后备匹配，或用户从同名候选中选定）。
    /// 设备设置、通道条、连线都保留。
    pub fn replace_device(&mut self, id: NodeId, device: DeviceRef) -> Result<(), EditError> {
        let role = self.node(id).ok_or(EditError::UnknownNode(id))?.kind.endpoint_role();
        let existing = self.nodes.iter().find(|node| {
            node.id != id
                && node.kind.endpoint_role() == role
                && node.kind.device().is_some_and(|own| own.endpoint == device.endpoint)
        });
        if let Some(node) = existing {
            return Err(EditError::DeviceInUse { by: node.id });
        }
        match &mut self.node_mut(id)?.kind {
            NodeKind::ExternalInput { device: own }
            | NodeKind::ExternalOutput { device: own }
            | NodeKind::ExternalInsert { ret: own, .. } => *own = device,
            _ => return Err(EditError::NotAnExternalNode(id)),
        }
        Ok(())
    }

    /// 设定外部设备节点的设备设置（独占、格式、缓冲、重采样算法）。
    pub fn set_device_settings(&mut self, id: NodeId, settings: DeviceSettings) -> Result<(), EditError> {
        let node = self.node_mut(id)?;
        if !node.kind.is_external() {
            return Err(EditError::NotAnExternalNode(id));
        }
        node.io = Some(settings);
        Ok(())
    }

    /// 设定录音节点的格式和位深；对下一次录音生效。
    pub fn set_recorder_settings(&mut self, id: NodeId, wanted: RecorderSettings) -> Result<(), EditError> {
        if !wanted.is_valid() {
            return Err(EditError::InvalidRecorderSettings);
        }
        match &mut self.node_mut(id)?.kind {
            NodeKind::Recorder { settings } => *settings = wanted,
            _ => return Err(EditError::NotARecorder(id)),
        }
        Ok(())
    }

    /// 让应用源改绑另一个程序（节点设置里"重新选择程序"）；连线和通道条保留。
    pub fn replace_program(&mut self, id: NodeId, replacement: ProgramPath) -> Result<(), EditError> {
        self.check_program(&replacement, Some(id))?;
        match &mut self.node_mut(id)?.kind {
            NodeKind::AppSource { program, .. } => *program = replacement,
            _ => return Err(EditError::NotAnAppSource(id)),
        }
        Ok(())
    }

    /// 设定应用源按节点覆盖的重采样算法；None 表示跟随全局默认。
    pub fn set_app_resampler(&mut self, id: NodeId, choice: Option<Resampler>) -> Result<(), EditError> {
        match &mut self.node_mut(id)?.kind {
            NodeKind::AppSource { resampler, .. } => *resampler = choice,
            _ => return Err(EditError::NotAnAppSource(id)),
        }
        Ok(())
    }

    /// 设定占用槽位的节点的格式与重采样算法。
    pub fn set_slot_settings(&mut self, id: NodeId, settings: SlotNodeSettings) -> Result<(), EditError> {
        let node = self.node_mut(id)?;
        if node.kind.slot().is_none() {
            return Err(EditError::NotASlotNode(id));
        }
        node.slot_settings = Some(settings);
        Ok(())
    }

    /// 每个被占用的槽位应有的格式，由占用它的节点决定。
    pub fn slot_formats(&self) -> BTreeMap<SlotId, SlotFormatChoice> {
        self.nodes
            .iter()
            .filter_map(|node| Some((node.kind.slot()?, node.slot_settings()?.format)))
            .collect()
    }

    /// 让虚拟节点改占另一个同方向的槽位（节点配置中改选）。
    pub fn change_slot(&mut self, id: NodeId, number: SlotNumber) -> Result<(), EditError> {
        let direction = self
            .node(id)
            .ok_or(EditError::UnknownNode(id))?
            .kind
            .slot_direction()
            .ok_or(EditError::NotAVirtualNode(id))?;
        let slot = SlotId { direction, number };
        if let Some(owner) = self.slot_owner(slot)
            && owner.id != id
        {
            return Err(EditError::SlotOccupied { slot, by: owner.id });
        }
        match &mut self.node_mut(id)?.kind {
            NodeKind::VirtualInput { slot: occupied }
            | NodeKind::VirtualOutput { slot: occupied }
            | NodeKind::ExternalInsert { send: occupied, .. } => *occupied = slot,
            _ => return Err(EditError::NotAVirtualNode(id)),
        }
        Ok(())
    }

    /// 检查能否从 `from` 连到 `to`（拖线时实时调用）。
    pub fn check_route(&self, from: NodeId, to: NodeId) -> Result<(), EditError> {
        let existing: Vec<(NodeId, NodeId)> = self.routes.iter().map(|route| (route.from, route.to)).collect();
        let role_of = |id: NodeId| self.node(id).map(|node| node.kind.role());
        Ok(topology::check_route(role_of, &existing, from, to)?)
    }

    /// 新建连线（0 dB、开启）。
    pub fn connect(&mut self, from: NodeId, to: NodeId) -> Result<(), EditError> {
        self.check_route(from, to)?;
        self.routes.push(Route {
            from,
            to,
            gain: GainDb::UNITY,
            enabled: true,
            label_position: LabelPosition::default(),
            label_visible: false,
            toggle_hotkey: None,
        });
        Ok(())
    }

    /// 删除连线，返回被删除的连线。
    pub fn disconnect(&mut self, from: NodeId, to: NodeId) -> Result<Route, EditError> {
        let index = self
            .routes
            .iter()
            .position(|route| route.from == from && route.to == to)
            .ok_or(EditError::UnknownRoute { from, to })?;
        Ok(self.routes.remove(index))
    }

    /// 设定连线的发送增益。
    pub fn set_route_gain(&mut self, from: NodeId, to: NodeId, gain: GainDb) -> Result<(), EditError> {
        self.route_mut(from, to)?.gain = gain;
        Ok(())
    }

    /// 开关连线。关闭的连线保留增益，只是不送声音。
    pub fn set_route_enabled(&mut self, from: NodeId, to: NodeId, enabled: bool) -> Result<(), EditError> {
        self.route_mut(from, to)?.enabled = enabled;
        Ok(())
    }

    /// 设定连线标签在连线上的位置。只影响显示。
    pub fn set_route_label(&mut self, from: NodeId, to: NodeId, position: LabelPosition) -> Result<(), EditError> {
        self.route_mut(from, to)?.label_position = position;
        Ok(())
    }

    /// 显示或隐藏连线标签。只影响显示。
    pub fn set_route_label_visible(&mut self, from: NodeId, to: NodeId, visible: bool) -> Result<(), EditError> {
        self.route_mut(from, to)?.label_visible = visible;
        Ok(())
    }

    /// 设定主时钟；必须是外部设备节点。None 表示跟随系统时钟。
    pub fn set_clock_master(&mut self, master: Option<NodeId>) -> Result<(), EditError> {
        if let Some(id) = master {
            let node = self.node(id).ok_or(EditError::UnknownNode(id))?;
            if !node.kind.can_be_clock_master() {
                return Err(EditError::InvalidClockMaster);
            }
        }
        self.engine.clock_master = master;
        Ok(())
    }

    /// 同一台外部设备在同一方向上只能被一个节点使用，否则两个 IO 线程会争用它。
    fn check_device(&self, device: &DeviceRef, role: NodeRole) -> Result<(), EditError> {
        let existing = self.nodes.iter().find(|node| {
            node.kind.endpoint_role() == Some(role)
                && node.kind.device().is_some_and(|own| own.endpoint == device.endpoint)
        });
        match existing {
            Some(node) => Err(EditError::DeviceInUse { by: node.id }),
            None => Ok(()),
        }
    }

    /// 每个程序只能有一个应用源；`except` 是正在改绑的节点自己。
    fn check_program(&self, program: &ProgramPath, except: Option<NodeId>) -> Result<(), EditError> {
        let existing = self
            .nodes
            .iter()
            .find(|node| Some(node.id) != except && node.kind.program().is_some_and(|own| own.same_program(program)));
        match existing {
            Some(node) => Err(EditError::ProgramInUse { by: node.id }),
            None => Ok(()),
        }
    }

    /// 为新节点挑槽位：指定了编号就检查它是否空闲，否则取编号最小的空闲槽位。
    fn claim_slot(&self, direction: SlotDirection, number: Option<SlotNumber>) -> Result<SlotId, EditError> {
        let Some(number) = number else {
            return self.free_slot(direction).ok_or(EditError::NoFreeSlot(direction));
        };
        let slot = SlotId { direction, number };
        match self.slot_owner(slot) {
            Some(owner) => Err(EditError::SlotOccupied { slot, by: owner.id }),
            None => Ok(slot),
        }
    }

    pub(crate) fn mute_hotkey_mut(&mut self, id: NodeId) -> Option<&mut Option<HotkeyBinding>> {
        self.nodes
            .iter_mut()
            .find(|node| node.id == id)
            .map(|node| &mut node.mute_hotkey)
    }

    pub(crate) fn record_hotkey_mut(&mut self, id: NodeId) -> Option<&mut Option<HotkeyBinding>> {
        self.nodes
            .iter_mut()
            .find(|node| node.id == id && matches!(node.kind, NodeKind::Recorder { .. }))
            .map(|node| &mut node.record_hotkey)
    }

    pub(crate) fn route_hotkey_mut(&mut self, from: NodeId, to: NodeId) -> Option<&mut Option<HotkeyBinding>> {
        self.routes
            .iter_mut()
            .find(|route| route.from == from && route.to == to)
            .map(|route| &mut route.toggle_hotkey)
    }

    fn index_of(&self, id: NodeId) -> Result<usize, EditError> {
        self.nodes
            .iter()
            .position(|node| node.id == id)
            .ok_or(EditError::UnknownNode(id))
    }

    fn node_mut(&mut self, id: NodeId) -> Result<&mut Node, EditError> {
        self.nodes
            .iter_mut()
            .find(|node| node.id == id)
            .ok_or(EditError::UnknownNode(id))
    }

    fn route_mut(&mut self, from: NodeId, to: NodeId) -> Result<&mut Route, EditError> {
        self.routes
            .iter_mut()
            .find(|route| route.from == from && route.to == to)
            .ok_or(EditError::UnknownRoute { from, to })
    }

    /// 读配置时的全部检查：ID 一致，编译出的路由拓扑通过校验。编辑后也要过这一关，否则保存的配置下次读不回来。
    pub fn check(&self) -> Result<(), EditError> {
        if !self.ids_are_consistent() {
            return Err(EditError::Invalid("a node ID is not below the next node ID".to_owned()));
        }
        self.compile(TopologyRevision(0))
            .validate()
            .map_err(|error| EditError::Invalid(error.to_string()))
    }

    /// 配置文件读入后的一致性：节点 ID 不能达到或超过下一个待分配的 ID；控制台只引用存在的节点、且不重复。
    /// 其余结构规则（槽位、连线、主时钟）由编译后的路由拓扑统一校验，见 [`Self::check`]。
    fn ids_are_consistent(&self) -> bool {
        let pins_valid = self
            .console
            .iter()
            .enumerate()
            .all(|(index, id)| self.node(*id).is_some() && !self.console[..index].contains(id));
        pins_valid && self.nodes.iter().all(|node| node.id.0 < self.next_node_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aemeath_protocol::ids::EndpointId;

    fn name(text: &str) -> NodeName {
        NodeName::new(text).unwrap()
    }

    fn device(id: &str) -> DeviceRef {
        DeviceRef {
            endpoint: EndpointId(id.to_owned()),
            name: id.to_owned(),
        }
    }

    fn add(workspace: &mut Workspace, text: &str, spec: NodeSpec) -> NodeId {
        workspace.add_node(name(text), spec, Position::default()).unwrap()
    }

    fn slot(direction: SlotDirection, number: u8) -> SlotId {
        SlotId {
            direction,
            number: SlotNumber::new(number).unwrap(),
        }
    }

    #[test]
    fn virtual_nodes_take_the_lowest_free_slot() {
        let mut workspace = Workspace::new(EngineSettings::default());
        let first = add(&mut workspace, "游戏", NodeSpec::VirtualInput(None));
        let second = add(&mut workspace, "游戏", NodeSpec::VirtualInput(None));
        let obs = add(&mut workspace, "OBS", NodeSpec::VirtualOutput(None));
        assert_eq!(
            workspace.node(first).unwrap().kind.slot(),
            Some(slot(SlotDirection::In, 1))
        );
        assert_eq!(
            workspace.node(second).unwrap().kind.slot(),
            Some(slot(SlotDirection::In, 2))
        );
        assert_eq!(
            workspace.node(obs).unwrap().kind.slot(),
            Some(slot(SlotDirection::Out, 1))
        );

        workspace.remove_node(first).unwrap();
        let third = add(&mut workspace, "Discord", NodeSpec::VirtualInput(None));
        assert_eq!(
            workspace.node(third).unwrap().kind.slot(),
            Some(slot(SlotDirection::In, 1))
        );
    }

    #[test]
    fn slots_cannot_be_shared_or_exhausted() {
        let mut workspace = Workspace::new(EngineSettings::default());
        let owner = add(&mut workspace, "A", NodeSpec::VirtualOutput(SlotNumber::new(3).ok()));
        assert_eq!(
            workspace.add_node(
                name("B"),
                NodeSpec::VirtualOutput(SlotNumber::new(3).ok()),
                Position::default()
            ),
            Err(EditError::SlotOccupied {
                slot: slot(SlotDirection::Out, 3),
                by: owner,
            })
        );
        for index in 0..7 {
            add(&mut workspace, &format!("Out {index}"), NodeSpec::VirtualOutput(None));
        }
        assert_eq!(
            workspace.add_node(name("too many"), NodeSpec::VirtualOutput(None), Position::default()),
            Err(EditError::NoFreeSlot(SlotDirection::Out))
        );
    }

    #[test]
    fn change_slot_moves_a_node_to_another_free_slot() {
        let mut workspace = Workspace::new(EngineSettings::default());
        let a = add(&mut workspace, "A", NodeSpec::VirtualInput(None));
        let b = add(&mut workspace, "B", NodeSpec::VirtualInput(None));
        assert_eq!(
            workspace.change_slot(a, SlotNumber::new(2).unwrap()),
            Err(EditError::SlotOccupied {
                slot: slot(SlotDirection::In, 2),
                by: b,
            })
        );
        workspace.change_slot(a, SlotNumber::new(5).unwrap()).unwrap();
        assert_eq!(workspace.node(a).unwrap().kind.slot(), Some(slot(SlotDirection::In, 5)));
        let bus = add(&mut workspace, "Bus", NodeSpec::Bus);
        assert_eq!(
            workspace.change_slot(bus, SlotNumber::new(6).unwrap()),
            Err(EditError::NotAVirtualNode(bus))
        );
    }

    #[test]
    fn check_catches_what_loading_would_reject() {
        use aemeath_protocol::message::SampleFormat;
        use aemeath_protocol::units::SampleRate;

        let mut workspace = Workspace::new(EngineSettings::default());
        let game = add(&mut workspace, "Game", NodeSpec::VirtualInput(None));
        assert_eq!(workspace.check(), Ok(()));
        // 编辑层只看节点类型，不看格式；读配置时的校验会拒绝 384 kHz 以上的槽位格式
        let too_fast = SlotNodeSettings {
            format: SlotFormatChoice::Fixed {
                sample_rate: SampleRate::new(768_000).unwrap(),
                sample_format: SampleFormat::Int32,
            },
            resampler: None,
        };
        workspace.set_slot_settings(game, too_fast).unwrap();
        assert!(matches!(workspace.check(), Err(EditError::Invalid(_))));
    }

    #[test]
    fn slot_settings_belong_to_the_node_not_the_slot() {
        use aemeath_protocol::message::SampleFormat;
        use aemeath_protocol::units::SampleRate;

        let mut workspace = Workspace::new(EngineSettings::default());
        let game = add(&mut workspace, "Game", NodeSpec::VirtualInput(None));
        let first = slot(SlotDirection::In, 1);
        let moved = slot(SlotDirection::In, 5);
        let format = SlotFormatChoice::Fixed {
            sample_rate: SampleRate::new(44_100).unwrap(),
            sample_format: SampleFormat::Int16,
        };
        let settings = SlotNodeSettings {
            format,
            resampler: None,
        };
        assert_eq!(
            workspace.slot_formats().get(&first),
            Some(&SlotFormatChoice::FollowEngine)
        );
        workspace.set_slot_settings(game, settings).unwrap();
        assert_eq!(workspace.slot_formats().get(&first), Some(&format));

        // 换槽位时带到新槽位，旧槽位不留设置
        workspace.change_slot(game, SlotNumber::new(5).unwrap()).unwrap();
        assert_eq!(workspace.slot_formats().get(&moved), Some(&format));
        assert_eq!(workspace.slot_formats().get(&first), None);

        // 删除后槽位空出，设置不留在槽位上
        workspace.remove_node(game).unwrap();
        assert!(workspace.slot_formats().is_empty());

        let bus = add(&mut workspace, "Bus", NodeSpec::Bus);
        assert_eq!(
            workspace.set_slot_settings(bus, settings),
            Err(EditError::NotASlotNode(bus))
        );
    }

    #[test]
    fn an_external_device_is_used_once_per_direction() {
        let mut workspace = Workspace::new(EngineSettings::default());
        let mic = add(&mut workspace, "Mic", NodeSpec::ExternalInput(device("mic")));
        assert_eq!(
            workspace.add_node(
                name("Mic 2"),
                NodeSpec::ExternalInput(device("mic")),
                Position::default()
            ),
            Err(EditError::DeviceInUse { by: mic })
        );
        // 同一个端点 ID 在另一方向上不冲突（实际不会出现，只说明规则按方向区分）
        assert!(
            workspace
                .add_node(
                    name("Out"),
                    NodeSpec::ExternalOutput(device("mic")),
                    Position::default()
                )
                .is_ok()
        );
    }

    #[test]
    fn a_program_has_one_app_source_and_can_be_replaced() {
        let program = |path: &str| ProgramPath::new(path).unwrap();
        let mut workspace = Workspace::new(EngineSettings::default());
        let chrome = add(
            &mut workspace,
            "Chrome",
            NodeSpec::AppSource(program(r"C:\Chrome\chrome.exe")),
        );
        let game = add(&mut workspace, "Game", NodeSpec::AppSource(program(r"D:\game.exe")));
        assert_eq!(
            workspace.add_node(
                name("Chrome 2"),
                NodeSpec::AppSource(program(r"c:\chrome\CHROME.EXE")),
                Position::default()
            ),
            Err(EditError::ProgramInUse { by: chrome })
        );
        assert_eq!(
            workspace.replace_program(game, program(r"C:\Chrome\chrome.exe")),
            Err(EditError::ProgramInUse { by: chrome })
        );
        workspace
            .replace_program(chrome, program(r"C:\Chrome\CHROME.exe"))
            .unwrap();
        workspace.replace_program(game, program(r"D:\new.exe")).unwrap();
        let fastest = Resampler::PolyAsync {
            preset: topology::PolyPreset::Fastest,
        };
        workspace.set_app_resampler(game, Some(fastest)).unwrap();
        assert_eq!(
            workspace.node(game).unwrap().kind,
            NodeKind::AppSource {
                program: program(r"D:\new.exe"),
                resampler: Some(fastest),
            }
        );
        let bus = add(&mut workspace, "Bus", NodeSpec::Bus);
        assert_eq!(
            workspace.replace_program(bus, program(r"D:\x.exe")),
            Err(EditError::NotAnAppSource(bus))
        );
        assert_eq!(
            workspace.set_clock_master(Some(game)),
            Err(EditError::InvalidClockMaster)
        );
    }

    #[test]
    fn an_external_insert_claims_a_send_slot_and_its_return_device() {
        let mut workspace = Workspace::new(EngineSettings::default());
        let obs = add(&mut workspace, "OBS", NodeSpec::VirtualOutput(None));
        let insert = add(
            &mut workspace,
            "Broadcast",
            NodeSpec::ExternalInsert {
                ret: device("broadcast"),
                send: None,
            },
        );
        assert_eq!(
            workspace.node(insert).unwrap().kind.slot(),
            Some(slot(SlotDirection::Out, 2)),
            "takes the lowest free Out"
        );
        assert!(
            workspace.node(insert).unwrap().io.is_some(),
            "the return has device settings"
        );
        assert_eq!(
            workspace.add_node(
                name("Direct"),
                NodeSpec::ExternalInput(device("broadcast")),
                Position::default()
            ),
            Err(EditError::DeviceInUse { by: insert })
        );
        workspace.change_slot(insert, SlotNumber::new(5).unwrap()).unwrap();
        assert_eq!(
            workspace.node(insert).unwrap().kind.slot(),
            Some(slot(SlotDirection::Out, 5))
        );
        assert_eq!(
            workspace.change_slot(insert, SlotNumber::new(1).unwrap()),
            Err(EditError::SlotOccupied {
                slot: slot(SlotDirection::Out, 1),
                by: obs,
            })
        );
        workspace.replace_device(insert, device("broadcast-2")).unwrap();
        assert_eq!(
            workspace.set_clock_master(Some(insert)),
            Err(EditError::InvalidClockMaster)
        );
        workspace.remove_node(insert).unwrap();
        assert_eq!(
            workspace.free_slot(SlotDirection::Out),
            Some(slot(SlotDirection::Out, 2))
        );
    }

    #[test]
    fn routes_are_checked_with_a_reason() {
        let mut workspace = Workspace::new(EngineSettings::default());
        let mic = add(&mut workspace, "Mic", NodeSpec::ExternalInput(device("mic")));
        let bus = add(&mut workspace, "Bus", NodeSpec::Bus);
        let second_bus = add(&mut workspace, "Bus 2", NodeSpec::Bus);
        let obs = add(&mut workspace, "OBS", NodeSpec::VirtualOutput(None));

        assert_eq!(workspace.check_route(bus, bus), Err(RouteRejection::SelfRoute.into()));
        assert_eq!(workspace.check_route(obs, bus), Err(RouteRejection::CannotSend.into()));
        assert_eq!(
            workspace.check_route(bus, mic),
            Err(RouteRejection::CannotReceive.into())
        );
        workspace.connect(mic, bus).unwrap();
        assert_eq!(workspace.check_route(mic, bus), Err(RouteRejection::Duplicate.into()));
        workspace.connect(bus, second_bus).unwrap();
        workspace.set_route_enabled(bus, second_bus, false).unwrap();
        assert_eq!(
            workspace.check_route(second_bus, bus),
            Err(RouteRejection::Loop.into()),
            "disabled routes still count for loops"
        );
        assert_eq!(
            workspace.check_route(mic, NodeId(99)),
            Err(EditError::UnknownNode(NodeId(99)))
        );
        assert!(workspace.check_route(second_bus, obs).is_ok());
    }

    #[test]
    fn removing_a_node_removes_its_routes_and_clock_master() {
        let mut workspace = Workspace::new(EngineSettings::default());
        let headphones = add(&mut workspace, "Headphones", NodeSpec::ExternalOutput(device("hp")));
        let game = add(&mut workspace, "Game", NodeSpec::VirtualInput(None));
        workspace.connect(game, headphones).unwrap();
        workspace.set_clock_master(Some(headphones)).unwrap();

        workspace.remove_node(headphones).unwrap();
        assert!(workspace.routes().is_empty());
        assert_eq!(workspace.engine.clock_master, None);
    }

    #[test]
    fn node_ids_are_never_reused() {
        let mut workspace = Workspace::new(EngineSettings::default());
        let first = add(&mut workspace, "A", NodeSpec::Bus);
        workspace.remove_node(first).unwrap();
        let second = add(&mut workspace, "B", NodeSpec::Bus);
        assert_ne!(first, second);
    }

    #[test]
    fn clock_master_must_be_external() {
        let mut workspace = Workspace::new(EngineSettings::default());
        let bus = add(&mut workspace, "Bus", NodeSpec::Bus);
        assert_eq!(
            workspace.set_clock_master(Some(bus)),
            Err(EditError::InvalidClockMaster)
        );
        let mic = add(&mut workspace, "Mic", NodeSpec::ExternalInput(device("mic")));
        workspace.set_clock_master(Some(mic)).unwrap();
        workspace.set_clock_master(None).unwrap();
    }

    #[test]
    fn pins_keep_their_order_and_follow_deletions() {
        let mut workspace = Workspace::new(EngineSettings::default());
        let mic = add(&mut workspace, "Mic", NodeSpec::ExternalInput(device("mic")));
        let bus = add(&mut workspace, "Bus", NodeSpec::Bus);
        let game = add(&mut workspace, "Game", NodeSpec::VirtualInput(None));
        workspace.pin(mic).unwrap();
        workspace.pin(bus).unwrap();
        workspace.pin(game).unwrap();
        workspace.pin(mic).unwrap();
        assert_eq!(workspace.console(), &[mic, bus, game]);
        workspace.move_pin(game, 0).unwrap();
        assert_eq!(workspace.console(), &[game, mic, bus]);
        workspace.move_pin(game, 99).unwrap();
        assert_eq!(workspace.console(), &[mic, bus, game]);
        workspace.remove_node(bus).unwrap();
        assert_eq!(workspace.console(), &[mic, game]);
        workspace.unpin(mic);
        assert_eq!(workspace.console(), &[game]);
        assert_eq!(workspace.pin(NodeId(99)), Err(EditError::UnknownNode(NodeId(99))));
        assert!(workspace.ids_are_consistent());
    }

    #[test]
    fn a_device_can_be_replaced_unless_another_node_uses_it() {
        let mut workspace = Workspace::new(EngineSettings::default());
        let mic = add(&mut workspace, "Mic", NodeSpec::ExternalInput(device("mic")));
        let other = add(&mut workspace, "Other", NodeSpec::ExternalInput(device("other")));
        workspace.replace_device(mic, device("mic-new-port")).unwrap();
        assert_eq!(
            workspace.node(mic).unwrap().kind.device().unwrap().endpoint,
            EndpointId("mic-new-port".to_owned())
        );
        assert_eq!(
            workspace.replace_device(mic, device("other")),
            Err(EditError::DeviceInUse { by: other })
        );
        let bus = add(&mut workspace, "Bus", NodeSpec::Bus);
        assert_eq!(
            workspace.replace_device(bus, device("x")),
            Err(EditError::NotAnExternalNode(bus))
        );
    }

    #[test]
    fn only_input_nodes_take_a_gate() {
        let mut workspace = Workspace::new(EngineSettings::default());
        let mic = add(&mut workspace, "Mic", NodeSpec::ExternalInput(device("mic")));
        let bus = add(&mut workspace, "Bus", NodeSpec::Bus);
        let gated = Strip {
            gate: Some(aemeath_protocol::units::GateThreshold::new(-45.0).unwrap()),
            ..Strip::default()
        };
        workspace.set_strip(mic, gated).unwrap();
        assert_eq!(workspace.node(mic).unwrap().strip, gated);
        assert_eq!(workspace.set_strip(bus, gated), Err(EditError::GateNotAllowed(bus)));
    }
}
