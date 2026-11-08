//! 工作台：节点、连线与引擎设置，以及对它们的全部编辑操作。
//!
//! 字段只能通过方法修改，以保证规则始终成立：
//! - 每个槽位最多被一个节点占用，方向与节点类型一致（ADR-0009）；新建虚拟节点时自动分配编号最小的空闲槽位。
//! - 同一个外部设备在同一方向上只对应一个节点。
//! - 连线规则与路由拓扑校验相同（[`topology::check_route`]）；拒绝时给出原因，供工作台拖线时提示。
//! - 节点 ID 只增不减、不复用（引擎换拓扑时按 ID 接过旧状态，复用 ID 会让新节点继承旧节点的增益）。

use aemeath_protocol::ids::{NodeId, SlotDirection, SlotId, SlotNumber};
use aemeath_protocol::topology::{self, NodeRole, RouteCheckError, RouteRejection, Strip};
use aemeath_protocol::units::GainDb;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::{DeviceRef, EngineSettings, LabelPosition, Node, NodeKind, NodeName, Position, Route};

/// 第一个节点的 ID
const FIRST_NODE_ID: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    pub engine: EngineSettings,
    nodes: Vec<Node>,
    routes: Vec<Route>,
    /// 下一个新节点的 ID（只增不减）
    next_node_id: u32,
}

/// 新建节点时的类型；虚拟节点可以指定槽位编号，不指定时自动分配。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeSpec {
    ExternalInput(DeviceRef),
    ExternalOutput(DeviceRef),
    VirtualInput(Option<SlotNumber>),
    VirtualOutput(Option<SlotNumber>),
    Bus,
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
    #[error("the clock master must be an external device node")]
    InvalidClockMaster,
    #[error("cannot route: {0}")]
    Route(#[from] RouteRejection),
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
        }
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
        };
        let id = NodeId(self.next_node_id);
        self.next_node_id += 1;
        self.nodes.push(Node {
            id,
            name,
            kind,
            strip: Strip::default(),
            position,
        });
        Ok(id)
    }

    /// 删除节点，连同与它相连的连线；它是主时钟时清除主时钟设置。它占用的槽位随之空出（拔出）。
    pub fn remove_node(&mut self, id: NodeId) -> Result<Node, EditError> {
        let index = self.index_of(id)?;
        self.routes.retain(|route| route.from != id && route.to != id);
        if self.engine.clock_master == Some(id) {
            self.engine.clock_master = None;
        }
        Ok(self.nodes.remove(index))
    }

    pub fn rename_node(&mut self, id: NodeId, name: NodeName) -> Result<(), EditError> {
        self.node_mut(id)?.name = name;
        Ok(())
    }

    pub fn move_node(&mut self, id: NodeId, position: Position) -> Result<(), EditError> {
        self.node_mut(id)?.position = position;
        Ok(())
    }

    pub fn set_strip(&mut self, id: NodeId, strip: Strip) -> Result<(), EditError> {
        self.node_mut(id)?.strip = strip;
        Ok(())
    }

    /// 让虚拟节点改占另一个同方向的槽位（节点配置中改选，ADR-0009）。
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
            NodeKind::VirtualInput { slot: occupied } | NodeKind::VirtualOutput { slot: occupied } => *occupied = slot,
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
        });
        Ok(())
    }

    /// 删除连线，返回被删除的连线（供撤销）。
    pub fn disconnect(&mut self, from: NodeId, to: NodeId) -> Result<Route, EditError> {
        let index = self
            .routes
            .iter()
            .position(|route| route.from == from && route.to == to)
            .ok_or(EditError::UnknownRoute { from, to })?;
        Ok(self.routes.remove(index))
    }

    /// 恢复一条被删除的连线（撤销）。仍按连线规则检查，因为期间拓扑可能已经变了。
    pub fn restore_route(&mut self, route: Route) -> Result<(), EditError> {
        self.check_route(route.from, route.to)?;
        self.routes.push(route);
        Ok(())
    }

    pub fn set_route_gain(&mut self, from: NodeId, to: NodeId, gain: GainDb) -> Result<(), EditError> {
        self.route_mut(from, to)?.gain = gain;
        Ok(())
    }

    pub fn set_route_enabled(&mut self, from: NodeId, to: NodeId, enabled: bool) -> Result<(), EditError> {
        self.route_mut(from, to)?.enabled = enabled;
        Ok(())
    }

    pub fn set_route_label(&mut self, from: NodeId, to: NodeId, position: LabelPosition) -> Result<(), EditError> {
        self.route_mut(from, to)?.label_position = position;
        Ok(())
    }

    /// 设定主时钟；必须是外部设备节点。None 表示跟随系统时钟。
    pub fn set_clock_master(&mut self, master: Option<NodeId>) -> Result<(), EditError> {
        if let Some(id) = master {
            let node = self.node(id).ok_or(EditError::UnknownNode(id))?;
            if !node.kind.is_external() {
                return Err(EditError::InvalidClockMaster);
            }
        }
        self.engine.clock_master = master;
        Ok(())
    }

    fn check_device(&self, device: &DeviceRef, role: NodeRole) -> Result<(), EditError> {
        let existing = self.nodes.iter().find(|node| {
            node.kind.role() == role && node.kind.device().is_some_and(|own| own.endpoint == device.endpoint)
        });
        match existing {
            Some(node) => Err(EditError::DeviceInUse { by: node.id }),
            None => Ok(()),
        }
    }

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

    /// 恢复到之前的快照（撤销）。保留两者中较大的"下一个节点 ID"：撤销新建节点之后，ID 也不会被再次分配。
    pub fn restore(&mut self, snapshot: Workspace) {
        let next_node_id = self.next_node_id.max(snapshot.next_node_id);
        *self = snapshot;
        self.next_node_id = next_node_id;
    }

    /// 配置文件读入后的一致性：节点 ID 不能达到或超过下一个待分配的 ID。
    /// 其余结构规则（槽位、连线、主时钟）由编译后的路由拓扑统一校验，见 [`crate::store`]。
    pub(crate) fn ids_are_consistent(&self) -> bool {
        self.nodes.iter().all(|node| node.id.0 < self.next_node_id)
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
        let first = add(&mut workspace, "默认输入", NodeSpec::VirtualInput(None));
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
    fn restoring_a_snapshot_keeps_node_ids_unique() {
        let mut workspace = Workspace::new(EngineSettings::default());
        let before = workspace.clone();
        let added = add(&mut workspace, "A", NodeSpec::Bus);
        workspace.restore(before);
        assert!(workspace.nodes().is_empty());
        let next = add(&mut workspace, "B", NodeSpec::Bus);
        assert_ne!(added, next, "an undone node's ID is not handed out again");
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
    fn deleted_routes_can_be_restored_unless_the_topology_changed() {
        let mut workspace = Workspace::new(EngineSettings::default());
        let a = add(&mut workspace, "A", NodeSpec::Bus);
        let b = add(&mut workspace, "B", NodeSpec::Bus);
        workspace.connect(a, b).unwrap();
        workspace.set_route_gain(a, b, GainDb::new(-6.0).unwrap()).unwrap();
        let removed = workspace.disconnect(a, b).unwrap();
        workspace.connect(b, a).unwrap();
        assert_eq!(workspace.restore_route(removed), Err(RouteRejection::Loop.into()));
        workspace.disconnect(b, a).unwrap();
        workspace.restore_route(removed).unwrap();
        assert_eq!(workspace.route(a, b).unwrap().gain.db(), -6.0);
    }
}
