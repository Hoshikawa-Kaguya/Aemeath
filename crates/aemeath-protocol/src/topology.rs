//! 路由拓扑：主程序从工作台的图编译出来、整体下发给引擎的完整拓扑与参数（ADR-0003）。
//!
//! 引擎只执行拓扑，不拥有配置；它缓存最近一份拓扑，重启后先按缓存恢复出声（ADR-0008）。
//! 拓扑总是整体替换，不做增量修改，这样引擎侧的原子替换和崩溃恢复都更简单。
//!
//! 节点类型 [`NodeKind`] 与连线规则 [`check_route`] 在这里只定义一次：主程序的工作台（`aemeath-core`）
//! 用同一个 `NodeKind`（外部设备带上设备名），拖线检查与拓扑校验用同一套规则。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::graph;
use crate::ids::{EndpointId, NodeId, SlotDirection, SlotId, TopologyRevision};
use crate::units::{GainDb, SampleRate};

/// 一份拓扑里节点数的上限。只用来拒绝异常输入：16 个槽位加上外部设备和总线，远小于这个数。
pub const MAX_NODES: usize = 256;
/// 一份拓扑里连线数的上限。
pub const MAX_ROUTES: usize = 4096;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteTopology {
    pub revision: TopologyRevision,
    /// 引擎采样率：内部统一处理用的全局采样率，每个 IO 节点在边界处转换到它
    pub engine_sample_rate: SampleRate,
    /// 主时钟：引擎跟随其时钟运行的外部设备节点；为 None 时跟随系统时钟（QPC）
    pub clock_master: Option<NodeId>,
    /// 全局默认的重采样算法（M1 不支持按节点覆盖）
    pub resampler: Resampler,
    pub nodes: Vec<TopologyNode>,
    pub routes: Vec<TopologyRoute>,
}

/// 重采样算法的档位（S5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resampler {
    /// Sinc 高质量：附加延迟最大（48k→192k 约 2.7 ms）
    SincHigh,
    /// Sinc 标准：默认档
    #[default]
    SincStandard,
    /// 多项式插值：低频好、高频差
    Polynomial,
    /// 线性插值：开销最小，音质最差
    Linear,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TopologyNode {
    pub id: NodeId,
    pub kind: NodeKind,
    pub strip: Strip,
}

/// 节点类型。M1 只有 IO 节点和总线；应用源、外部插入、录音节点以后加入并提升协议版本。
///
/// `D` 是外部设备的表示：路由拓扑里只是端点 ID；主程序的工作台里还带上设备名，用于后备匹配。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum NodeKind<D = EndpointId> {
    /// 外部输入（A）：不专属于 Aemeath 的录音端点
    ExternalInput { device: D },
    /// 虚拟输入（B）：占用一个输入方向的槽位
    VirtualInput { slot: SlotId },
    /// 虚拟输出（C）：占用一个输出方向的槽位
    VirtualOutput { slot: SlotId },
    /// 外部输出（D）：不专属于 Aemeath 的播放端点
    ExternalOutput { device: D },
    /// 总线：只求和再分发
    Bus,
}

/// 节点在信号流中的角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeRole {
    Input,
    Processing,
    Output,
}

impl<D> NodeKind<D> {
    pub fn role(&self) -> NodeRole {
        match self {
            Self::ExternalInput { .. } | Self::VirtualInput { .. } => NodeRole::Input,
            Self::Bus => NodeRole::Processing,
            Self::VirtualOutput { .. } | Self::ExternalOutput { .. } => NodeRole::Output,
        }
    }

    /// 能否作为连线的起点（输出节点不发出声音）
    pub fn can_send(&self) -> bool {
        self.role() != NodeRole::Output
    }

    /// 能否作为连线的终点（输入节点不接收声音）
    pub fn can_receive(&self) -> bool {
        self.role() != NodeRole::Input
    }

    /// 节点占用的槽位
    pub fn slot(&self) -> Option<SlotId> {
        match self {
            Self::VirtualInput { slot } | Self::VirtualOutput { slot } => Some(*slot),
            _ => None,
        }
    }

    /// 占用槽位的节点类型要求的槽位方向
    pub fn slot_direction(&self) -> Option<SlotDirection> {
        match self {
            Self::VirtualInput { .. } => Some(SlotDirection::In),
            Self::VirtualOutput { .. } => Some(SlotDirection::Out),
            _ => None,
        }
    }

    /// 外部设备节点的设备
    pub fn device(&self) -> Option<&D> {
        match self {
            Self::ExternalInput { device } | Self::ExternalOutput { device } => Some(device),
            _ => None,
        }
    }

    pub fn is_external(&self) -> bool {
        self.device().is_some()
    }

    /// 换一种外部设备的表示（例如从工作台的设备记录取出端点 ID），其余不变。
    pub fn map_device<E>(&self, map: impl FnOnce(&D) -> E) -> NodeKind<E> {
        match self {
            Self::ExternalInput { device } => NodeKind::ExternalInput { device: map(device) },
            Self::VirtualInput { slot } => NodeKind::VirtualInput { slot: *slot },
            Self::VirtualOutput { slot } => NodeKind::VirtualOutput { slot: *slot },
            Self::ExternalOutput { device } => NodeKind::ExternalOutput { device: map(device) },
            Self::Bus => NodeKind::Bus,
        }
    }
}

/// 通道条参数。M1 只有增益和静音；门限和监听在 M2 加入。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Strip {
    pub gain: GainDb,
    pub muted: bool,
}

impl Default for Strip {
    fn default() -> Self {
        Self {
            gain: GainDb::UNITY,
            muted: false,
        }
    }
}

/// 一条连线：从 `from` 到 `to` 的有向发送。关闭（`enabled = false`）的连线保留配置，不同于删除。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TopologyRoute {
    pub from: NodeId,
    pub to: NodeId,
    pub gain: GainDb,
    pub enabled: bool,
}

/// 不能连线的原因（工作台拖线时显示，拓扑校验也用同一套规则）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RouteRejection {
    #[error("a node cannot be routed to itself")]
    SelfRoute,
    /// 起点是输出节点，不发出声音
    #[error("the source node does not send sound")]
    CannotSend,
    /// 终点是输入节点，不接收声音
    #[error("the destination node does not receive sound")]
    CannotReceive,
    #[error("the route already exists")]
    Duplicate,
    /// 关闭的连线也算在内：重新打开时不应该突然形成回路
    #[error("the route would create a loop")]
    Loop,
}

/// [`check_route`] 的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RouteCheckError {
    #[error("{0} does not exist")]
    UnknownNode(NodeId),
    #[error(transparent)]
    Rejected(#[from] RouteRejection),
}

/// 检查能否在已有连线 `existing` 之上新增 `from` → `to`。
/// `role_of` 查节点的角色，节点不存在时返回 None。
pub fn check_route(
    role_of: impl Fn(NodeId) -> Option<NodeRole>,
    existing: &[(NodeId, NodeId)],
    from: NodeId,
    to: NodeId,
) -> Result<(), RouteCheckError> {
    let source = role_of(from).ok_or(RouteCheckError::UnknownNode(from))?;
    let destination = role_of(to).ok_or(RouteCheckError::UnknownNode(to))?;
    let rejection = if from == to {
        RouteRejection::SelfRoute
    } else if source == NodeRole::Output {
        RouteRejection::CannotSend
    } else if destination == NodeRole::Input {
        RouteRejection::CannotReceive
    } else if existing.contains(&(from, to)) {
        RouteRejection::Duplicate
    } else if graph::reaches(existing, to, from) {
        RouteRejection::Loop
    } else {
        return Ok(());
    };
    Err(rejection.into())
}

/// 路由拓扑不合法的原因。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TopologyError {
    #[error("the topology has {0} nodes, more than the limit {MAX_NODES}")]
    TooManyNodes(usize),
    #[error("the topology has {0} routes, more than the limit {MAX_ROUTES}")]
    TooManyRoutes(usize),
    #[error("{0} appears more than once")]
    DuplicateNode(NodeId),
    #[error("{node} has an empty or overlong endpoint ID")]
    InvalidEndpoint { node: NodeId },
    #[error("{second} uses the same external endpoint in the same direction as {first}")]
    DuplicateEndpoint { first: NodeId, second: NodeId },
    #[error("{node} uses slot {slot}, which is in the wrong direction for its kind")]
    SlotDirectionMismatch { node: NodeId, slot: SlotId },
    #[error("slot {slot} is occupied by both {first} and {second}")]
    SlotOccupiedTwice {
        slot: SlotId,
        first: NodeId,
        second: NodeId,
    },
    #[error("the clock master {0} is not an external device node")]
    InvalidClockMaster(NodeId),
    #[error("a route refers to {0}, which is not in the topology")]
    UnknownNode(NodeId),
    #[error("the route from {from} to {to} is not allowed: {rejection}")]
    InvalidRoute {
        from: NodeId,
        to: NodeId,
        rejection: RouteRejection,
    },
}

impl RouteTopology {
    /// 检查拓扑是否可以执行。只检查结构；外部设备是否存在、能否打开由引擎运行时处理（离线状态）。
    pub fn validate(&self) -> Result<(), TopologyError> {
        if self.nodes.len() > MAX_NODES {
            return Err(TopologyError::TooManyNodes(self.nodes.len()));
        }
        if self.routes.len() > MAX_ROUTES {
            return Err(TopologyError::TooManyRoutes(self.routes.len()));
        }

        let nodes = self.validate_nodes()?;
        if let Some(master) = self.clock_master
            && !nodes.get(&master).is_some_and(|kind| kind.is_external())
        {
            return Err(TopologyError::InvalidClockMaster(master));
        }
        self.validate_routes(&nodes)
    }

    fn validate_nodes(&self) -> Result<HashMap<NodeId, &NodeKind>, TopologyError> {
        let mut nodes = HashMap::with_capacity(self.nodes.len());
        let mut slots: HashMap<SlotId, NodeId> = HashMap::new();
        // 同一个外部端点在同一方向上只能对应一个节点，否则两个 IO 线程会争用同一台设备
        let mut endpoints: HashMap<(&EndpointId, NodeRole), NodeId> = HashMap::new();
        for node in &self.nodes {
            if nodes.insert(node.id, &node.kind).is_some() {
                return Err(TopologyError::DuplicateNode(node.id));
            }
            if let Some(endpoint) = node.kind.device() {
                if !endpoint.is_well_formed() {
                    return Err(TopologyError::InvalidEndpoint { node: node.id });
                }
                if let Some(first) = endpoints.insert((endpoint, node.kind.role()), node.id) {
                    return Err(TopologyError::DuplicateEndpoint { first, second: node.id });
                }
            }
            if let (Some(slot), Some(required)) = (node.kind.slot(), node.kind.slot_direction()) {
                if slot.direction != required {
                    return Err(TopologyError::SlotDirectionMismatch { node: node.id, slot });
                }
                if let Some(first) = slots.insert(slot, node.id) {
                    return Err(TopologyError::SlotOccupiedTwice {
                        slot,
                        first,
                        second: node.id,
                    });
                }
            }
        }
        Ok(nodes)
    }

    /// 按顺序逐条检查连线（与工作台拖线时的规则相同），成环时指出闭合回路的那一条。
    fn validate_routes(&self, nodes: &HashMap<NodeId, &NodeKind>) -> Result<(), TopologyError> {
        let role_of = |id: NodeId| nodes.get(&id).map(|kind| kind.role());
        let mut accepted = Vec::with_capacity(self.routes.len());
        for route in &self.routes {
            match check_route(role_of, &accepted, route.from, route.to) {
                Ok(()) => accepted.push((route.from, route.to)),
                Err(RouteCheckError::UnknownNode(id)) => return Err(TopologyError::UnknownNode(id)),
                Err(RouteCheckError::Rejected(rejection)) => {
                    return Err(TopologyError::InvalidRoute {
                        from: route.from,
                        to: route.to,
                        rejection,
                    });
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::SlotNumber;

    const MIC: NodeId = NodeId(1);
    const GAME: NodeId = NodeId(2);
    const BUS: NodeId = NodeId(3);
    const OBS: NodeId = NodeId(4);
    const HEADPHONES: NodeId = NodeId(5);

    fn slot(direction: SlotDirection, number: u8) -> SlotId {
        SlotId {
            direction,
            number: SlotNumber::new(number).unwrap(),
        }
    }

    fn node(id: NodeId, kind: NodeKind) -> TopologyNode {
        TopologyNode {
            id,
            kind,
            strip: Strip::default(),
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

    fn invalid_route(from: NodeId, to: NodeId, rejection: RouteRejection) -> Result<(), TopologyError> {
        Err(TopologyError::InvalidRoute { from, to, rejection })
    }

    /// M1 验收场景：麦克风 → 虚拟输出（OBS），应用 → 虚拟输入 → 总线 → 耳机
    fn sample_topology() -> RouteTopology {
        RouteTopology {
            revision: TopologyRevision(1),
            engine_sample_rate: SampleRate::new(48_000).unwrap(),
            clock_master: Some(HEADPHONES),
            resampler: Resampler::default(),
            nodes: vec![
                node(
                    MIC,
                    NodeKind::ExternalInput {
                        device: EndpointId("{0.0.1.00000000}.{mic}".to_owned()),
                    },
                ),
                node(
                    GAME,
                    NodeKind::VirtualInput {
                        slot: slot(SlotDirection::In, 1),
                    },
                ),
                node(BUS, NodeKind::Bus),
                node(
                    OBS,
                    NodeKind::VirtualOutput {
                        slot: slot(SlotDirection::Out, 1),
                    },
                ),
                node(
                    HEADPHONES,
                    NodeKind::ExternalOutput {
                        device: EndpointId("{0.0.0.00000000}.{headphones}".to_owned()),
                    },
                ),
            ],
            routes: vec![
                route(MIC, OBS),
                route(GAME, BUS),
                route(BUS, HEADPHONES),
                route(BUS, OBS),
            ],
        }
    }

    #[test]
    fn sample_topology_is_valid() {
        assert_eq!(sample_topology().validate(), Ok(()));
    }

    #[test]
    fn rejects_duplicate_node_ids() {
        let mut topology = sample_topology();
        topology.nodes.push(node(BUS, NodeKind::Bus));
        assert_eq!(topology.validate(), Err(TopologyError::DuplicateNode(BUS)));
    }

    #[test]
    fn rejects_slot_in_wrong_direction() {
        let mut topology = sample_topology();
        topology.nodes[1].kind = NodeKind::VirtualInput {
            slot: slot(SlotDirection::Out, 2),
        };
        assert_eq!(
            topology.validate(),
            Err(TopologyError::SlotDirectionMismatch {
                node: GAME,
                slot: slot(SlotDirection::Out, 2),
            })
        );
    }

    #[test]
    fn rejects_slot_occupied_twice() {
        let mut topology = sample_topology();
        topology.nodes.push(node(
            NodeId(9),
            NodeKind::VirtualInput {
                slot: slot(SlotDirection::In, 1),
            },
        ));
        assert_eq!(
            topology.validate(),
            Err(TopologyError::SlotOccupiedTwice {
                slot: slot(SlotDirection::In, 1),
                first: GAME,
                second: NodeId(9),
            })
        );
    }

    #[test]
    fn rejects_empty_endpoint_id() {
        let mut topology = sample_topology();
        topology.nodes[0].kind = NodeKind::ExternalInput {
            device: EndpointId(String::new()),
        };
        assert_eq!(topology.validate(), Err(TopologyError::InvalidEndpoint { node: MIC }));
    }

    #[test]
    fn rejects_the_same_external_endpoint_twice_in_one_direction() {
        let mut topology = sample_topology();
        let second_mic = NodeId(9);
        topology.nodes.push(node(second_mic, topology.nodes[0].kind.clone()));
        assert_eq!(
            topology.validate(),
            Err(TopologyError::DuplicateEndpoint {
                first: MIC,
                second: second_mic,
            })
        );
    }

    #[test]
    fn clock_master_must_be_an_external_device() {
        let mut topology = sample_topology();
        topology.clock_master = Some(BUS);
        assert_eq!(topology.validate(), Err(TopologyError::InvalidClockMaster(BUS)));
        topology.clock_master = None;
        assert_eq!(topology.validate(), Ok(()));
    }

    #[test]
    fn rejects_routes_to_unknown_nodes() {
        let mut topology = sample_topology();
        topology.routes.push(route(MIC, NodeId(42)));
        assert_eq!(topology.validate(), Err(TopologyError::UnknownNode(NodeId(42))));
    }

    #[test]
    fn rejects_self_routes() {
        let mut topology = sample_topology();
        topology.routes.push(route(BUS, BUS));
        assert_eq!(topology.validate(), invalid_route(BUS, BUS, RouteRejection::SelfRoute));
    }

    #[test]
    fn rejects_routes_in_the_wrong_direction() {
        let mut topology = sample_topology();
        topology.routes.push(route(OBS, HEADPHONES));
        assert_eq!(
            topology.validate(),
            invalid_route(OBS, HEADPHONES, RouteRejection::CannotSend)
        );

        let mut topology = sample_topology();
        topology.routes.push(route(BUS, GAME));
        assert_eq!(
            topology.validate(),
            invalid_route(BUS, GAME, RouteRejection::CannotReceive)
        );
    }

    #[test]
    fn rejects_duplicate_routes() {
        let mut topology = sample_topology();
        topology.routes.push(route(MIC, OBS));
        assert_eq!(topology.validate(), invalid_route(MIC, OBS, RouteRejection::Duplicate));
    }

    #[test]
    fn rejects_loops_even_through_disabled_routes() {
        let mut topology = sample_topology();
        let second_bus = NodeId(6);
        topology.nodes.push(node(second_bus, NodeKind::Bus));
        topology.routes.push(route(BUS, second_bus));
        topology.routes.push(TopologyRoute {
            enabled: false,
            ..route(second_bus, BUS)
        });
        assert_eq!(
            topology.validate(),
            invalid_route(second_bus, BUS, RouteRejection::Loop)
        );
    }

    #[test]
    fn rejects_too_many_nodes() {
        let mut topology = sample_topology();
        let first_extra = 100;
        for id in first_extra..first_extra + MAX_NODES as u32 {
            topology.nodes.push(node(NodeId(id), NodeKind::Bus));
        }
        assert_eq!(
            topology.validate(),
            Err(TopologyError::TooManyNodes(topology.nodes.len()))
        );
    }

    #[test]
    fn check_route_reports_the_first_rule_that_fails() {
        let roles = [
            (MIC, NodeRole::Input),
            (BUS, NodeRole::Processing),
            (OBS, NodeRole::Output),
        ];
        let role_of = |id: NodeId| roles.iter().find(|(node, _)| *node == id).map(|(_, role)| *role);
        let existing = [(MIC, BUS)];
        assert_eq!(check_route(role_of, &existing, BUS, OBS), Ok(()));
        assert_eq!(
            check_route(role_of, &existing, MIC, BUS),
            Err(RouteRejection::Duplicate.into())
        );
        assert_eq!(
            check_route(role_of, &existing, OBS, MIC),
            Err(RouteRejection::CannotSend.into())
        );
        assert_eq!(
            check_route(role_of, &existing, BUS, NodeId(9)),
            Err(RouteCheckError::UnknownNode(NodeId(9)))
        );
    }

    #[test]
    fn node_kind_works_with_any_device_representation() {
        let kind: NodeKind<(String, &str)> = NodeKind::ExternalOutput {
            device: ("{id}".to_owned(), "Headphones"),
        };
        assert_eq!(kind.role(), NodeRole::Output);
        assert!(kind.is_external());
        assert_eq!(kind.slot(), None);
        let mapped = kind.map_device(|(id, _)| EndpointId(id.clone()));
        assert_eq!(
            mapped,
            NodeKind::ExternalOutput {
                device: EndpointId("{id}".to_owned()),
            }
        );
    }

    #[test]
    fn unknown_fields_are_rejected_when_deserializing() {
        let json = r#"{"id":1,"kind":{"type":"bus"},"strip":{"gain":0,"muted":false},"extra":true}"#;
        assert!(serde_json::from_str::<TopologyNode>(json).is_err());
        let kind_with_extra_field = r#"{"type":"virtual_input","slot":{"direction":"in","number":1},"extra":1}"#;
        assert!(serde_json::from_str::<NodeKind>(kind_with_extra_field).is_err());
    }

    #[test]
    fn topology_round_trips_through_json() {
        let topology = sample_topology();
        let json = serde_json::to_string(&topology).unwrap();
        assert_eq!(serde_json::from_str::<RouteTopology>(&json).unwrap(), topology);
    }

    #[test]
    fn node_kind_has_a_stable_wire_format() {
        let kind: NodeKind = NodeKind::VirtualOutput {
            slot: slot(SlotDirection::Out, 3),
        };
        assert_eq!(
            serde_json::to_string(&kind).unwrap(),
            r#"{"type":"virtual_output","slot":{"direction":"out","number":3}}"#
        );
        let external: NodeKind = NodeKind::ExternalInput {
            device: EndpointId("{mic}".to_owned()),
        };
        assert_eq!(
            serde_json::to_string(&external).unwrap(),
            r#"{"type":"external_input","device":"{mic}"}"#
        );
    }
}
