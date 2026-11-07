//! 路由计划：主程序从工作台的图编译出来、整体下发给引擎的完整拓扑与参数（ADR-0003）。
//!
//! 引擎只执行计划，不拥有配置；它缓存最近一份计划，重启后先按缓存恢复出声（ADR-0008）。
//! 计划总是整体替换，不做增量修改，这样引擎侧的原子替换和崩溃恢复都更简单。

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ids::{EndpointId, NodeId, PlanRevision, SlotDirection, SlotId};
use crate::units::{GainDb, SampleRate};

/// 一份计划里节点数的上限。只用来拒绝异常输入：16 个槽位加上外部设备和总线，远小于这个数。
pub const MAX_NODES: usize = 256;
/// 一份计划里连线数的上限。
pub const MAX_ROUTES: usize = 4096;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutePlan {
    pub revision: PlanRevision,
    /// 引擎采样率：内部统一处理用的全局采样率，每个 IO 节点在边界处转换到它
    pub engine_sample_rate: SampleRate,
    /// 主时钟：引擎跟随其时钟运行的外部设备节点；为 None 时跟随系统时钟（QPC）
    pub clock_master: Option<NodeId>,
    /// 全局默认的重采样算法（M1 不支持按节点覆盖）
    pub resampler: Resampler,
    pub nodes: Vec<PlanNode>,
    pub routes: Vec<PlanRoute>,
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
pub struct PlanNode {
    pub id: NodeId,
    pub kind: NodeKind,
    pub strip: Strip,
}

/// 节点类型。M1 只有 IO 节点和总线；应用源、外部插入、录音节点以后加入并提升协议版本。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum NodeKind {
    /// 外部输入（A）：不专属于 Aemeath 的录音端点
    ExternalInput { endpoint: EndpointId },
    /// 虚拟输入（B）：占用一个输入方向的槽位
    VirtualInput { slot: SlotId },
    /// 虚拟输出（C）：占用一个输出方向的槽位
    VirtualOutput { slot: SlotId },
    /// 外部输出（D）：不专属于 Aemeath 的播放端点
    ExternalOutput { endpoint: EndpointId },
    /// 总线：只求和再分发
    Bus,
}

/// 节点在信号流中的角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRole {
    Input,
    Processing,
    Output,
}

impl NodeKind {
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

    /// 节点占用的槽位，以及该节点类型要求的槽位方向
    fn slot(&self) -> Option<(SlotId, SlotDirection)> {
        match self {
            Self::VirtualInput { slot } => Some((*slot, SlotDirection::In)),
            Self::VirtualOutput { slot } => Some((*slot, SlotDirection::Out)),
            _ => None,
        }
    }

    fn endpoint(&self) -> Option<&EndpointId> {
        match self {
            Self::ExternalInput { endpoint } | Self::ExternalOutput { endpoint } => Some(endpoint),
            _ => None,
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
pub struct PlanRoute {
    pub from: NodeId,
    pub to: NodeId,
    pub gain: GainDb,
    pub enabled: bool,
}

/// 路由计划不合法的原因。前端拖动连线时的提示也基于其中与连线有关的几种。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PlanError {
    #[error("the plan has {0} nodes, more than the limit {MAX_NODES}")]
    TooManyNodes(usize),
    #[error("the plan has {0} routes, more than the limit {MAX_ROUTES}")]
    TooManyRoutes(usize),
    #[error("{0} appears more than once")]
    DuplicateNode(NodeId),
    #[error("{node} has an empty or overlong endpoint ID")]
    InvalidEndpoint { node: NodeId },
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
    #[error("a route refers to {0}, which is not in the plan")]
    UnknownNode(NodeId),
    #[error("{0} is routed to itself")]
    SelfRoute(NodeId),
    #[error("{0} is an output node and cannot send")]
    CannotSend(NodeId),
    #[error("{0} is an input node and cannot receive")]
    CannotReceive(NodeId),
    #[error("the route from {from} to {to} appears more than once")]
    DuplicateRoute { from: NodeId, to: NodeId },
    #[error("the routes form a loop through {0}")]
    Loop(NodeId),
}

impl RoutePlan {
    /// 检查计划是否可以执行。只检查结构；外部设备是否存在、能否打开由引擎运行时处理（离线状态）。
    pub fn validate(&self) -> Result<(), PlanError> {
        if self.nodes.len() > MAX_NODES {
            return Err(PlanError::TooManyNodes(self.nodes.len()));
        }
        if self.routes.len() > MAX_ROUTES {
            return Err(PlanError::TooManyRoutes(self.routes.len()));
        }

        let nodes = self.validate_nodes()?;
        if let Some(master) = self.clock_master {
            let is_external = matches!(
                nodes.get(&master),
                Some(NodeKind::ExternalInput { .. } | NodeKind::ExternalOutput { .. })
            );
            if !is_external {
                return Err(PlanError::InvalidClockMaster(master));
            }
        }

        self.validate_routes(&nodes)?;
        self.check_acyclic()
    }

    fn validate_nodes(&self) -> Result<HashMap<NodeId, &NodeKind>, PlanError> {
        let mut nodes = HashMap::with_capacity(self.nodes.len());
        let mut slots: HashMap<SlotId, NodeId> = HashMap::new();
        for node in &self.nodes {
            if nodes.insert(node.id, &node.kind).is_some() {
                return Err(PlanError::DuplicateNode(node.id));
            }
            if let Some(endpoint) = node.kind.endpoint()
                && !endpoint.is_well_formed()
            {
                return Err(PlanError::InvalidEndpoint { node: node.id });
            }
            if let Some((slot, required)) = node.kind.slot() {
                if slot.direction != required {
                    return Err(PlanError::SlotDirectionMismatch { node: node.id, slot });
                }
                if let Some(first) = slots.insert(slot, node.id) {
                    return Err(PlanError::SlotOccupiedTwice {
                        slot,
                        first,
                        second: node.id,
                    });
                }
            }
        }
        Ok(nodes)
    }

    fn validate_routes(&self, nodes: &HashMap<NodeId, &NodeKind>) -> Result<(), PlanError> {
        let mut seen = HashSet::with_capacity(self.routes.len());
        for route in &self.routes {
            let from = nodes.get(&route.from).ok_or(PlanError::UnknownNode(route.from))?;
            let to = nodes.get(&route.to).ok_or(PlanError::UnknownNode(route.to))?;
            if route.from == route.to {
                return Err(PlanError::SelfRoute(route.from));
            }
            if !from.can_send() {
                return Err(PlanError::CannotSend(route.from));
            }
            if !to.can_receive() {
                return Err(PlanError::CannotReceive(route.to));
            }
            if !seen.insert((route.from, route.to)) {
                return Err(PlanError::DuplicateRoute {
                    from: route.from,
                    to: route.to,
                });
            }
        }
        Ok(())
    }

    /// 用 Kahn 算法检查连线是否成环。关闭的连线也算在内：重新打开时不应该突然形成回路。
    fn check_acyclic(&self) -> Result<(), PlanError> {
        let mut incoming: HashMap<NodeId, usize> = self.nodes.iter().map(|node| (node.id, 0)).collect();
        let mut outgoing: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
        for route in &self.routes {
            *incoming.entry(route.to).or_default() += 1;
            outgoing.entry(route.from).or_default().push(route.to);
        }

        let mut ready: Vec<NodeId> = incoming
            .iter()
            .filter(|(_, count)| **count == 0)
            .map(|(id, _)| *id)
            .collect();
        let mut visited = 0;
        while let Some(node) = ready.pop() {
            visited += 1;
            for next in outgoing.get(&node).into_iter().flatten() {
                if let Some(count) = incoming.get_mut(next) {
                    *count -= 1;
                    if *count == 0 {
                        ready.push(*next);
                    }
                }
            }
        }

        if visited == incoming.len() {
            return Ok(());
        }
        // 没有被访问到的节点都在环上或在环的下游；取 ID 最小的一个，让错误信息稳定
        let stuck = incoming
            .iter()
            .filter(|(_, count)| **count > 0)
            .map(|(id, _)| *id)
            .min();
        match stuck {
            Some(node) => Err(PlanError::Loop(node)),
            None => Ok(()),
        }
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

    fn node(id: NodeId, kind: NodeKind) -> PlanNode {
        PlanNode {
            id,
            kind,
            strip: Strip::default(),
        }
    }

    fn route(from: NodeId, to: NodeId) -> PlanRoute {
        PlanRoute {
            from,
            to,
            gain: GainDb::UNITY,
            enabled: true,
        }
    }

    /// M1 验收场景：麦克风 → 虚拟输出（OBS），应用 → 虚拟输入 → 总线 → 耳机
    fn sample_plan() -> RoutePlan {
        RoutePlan {
            revision: PlanRevision(1),
            engine_sample_rate: SampleRate::new(48_000).unwrap(),
            clock_master: Some(HEADPHONES),
            resampler: Resampler::default(),
            nodes: vec![
                node(
                    MIC,
                    NodeKind::ExternalInput {
                        endpoint: EndpointId("{0.0.1.00000000}.{mic}".to_owned()),
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
                        endpoint: EndpointId("{0.0.0.00000000}.{headphones}".to_owned()),
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
    fn sample_plan_is_valid() {
        assert_eq!(sample_plan().validate(), Ok(()));
    }

    #[test]
    fn rejects_duplicate_node_ids() {
        let mut plan = sample_plan();
        plan.nodes.push(node(BUS, NodeKind::Bus));
        assert_eq!(plan.validate(), Err(PlanError::DuplicateNode(BUS)));
    }

    #[test]
    fn rejects_slot_in_wrong_direction() {
        let mut plan = sample_plan();
        plan.nodes[1].kind = NodeKind::VirtualInput {
            slot: slot(SlotDirection::Out, 2),
        };
        assert_eq!(
            plan.validate(),
            Err(PlanError::SlotDirectionMismatch {
                node: GAME,
                slot: slot(SlotDirection::Out, 2),
            })
        );
    }

    #[test]
    fn rejects_slot_occupied_twice() {
        let mut plan = sample_plan();
        plan.nodes.push(node(
            NodeId(9),
            NodeKind::VirtualInput {
                slot: slot(SlotDirection::In, 1),
            },
        ));
        assert_eq!(
            plan.validate(),
            Err(PlanError::SlotOccupiedTwice {
                slot: slot(SlotDirection::In, 1),
                first: GAME,
                second: NodeId(9),
            })
        );
    }

    #[test]
    fn rejects_empty_endpoint_id() {
        let mut plan = sample_plan();
        plan.nodes[0].kind = NodeKind::ExternalInput {
            endpoint: EndpointId(String::new()),
        };
        assert_eq!(plan.validate(), Err(PlanError::InvalidEndpoint { node: MIC }));
    }

    #[test]
    fn clock_master_must_be_an_external_device() {
        let mut plan = sample_plan();
        plan.clock_master = Some(BUS);
        assert_eq!(plan.validate(), Err(PlanError::InvalidClockMaster(BUS)));
        plan.clock_master = None;
        assert_eq!(plan.validate(), Ok(()));
    }

    #[test]
    fn rejects_routes_to_unknown_nodes() {
        let mut plan = sample_plan();
        plan.routes.push(route(MIC, NodeId(42)));
        assert_eq!(plan.validate(), Err(PlanError::UnknownNode(NodeId(42))));
    }

    #[test]
    fn rejects_self_routes() {
        let mut plan = sample_plan();
        plan.routes.push(route(BUS, BUS));
        assert_eq!(plan.validate(), Err(PlanError::SelfRoute(BUS)));
    }

    #[test]
    fn rejects_routes_in_the_wrong_direction() {
        let mut plan = sample_plan();
        plan.routes.push(route(OBS, HEADPHONES));
        assert_eq!(plan.validate(), Err(PlanError::CannotSend(OBS)));

        let mut plan = sample_plan();
        plan.routes.push(route(BUS, GAME));
        assert_eq!(plan.validate(), Err(PlanError::CannotReceive(GAME)));
    }

    #[test]
    fn rejects_duplicate_routes() {
        let mut plan = sample_plan();
        plan.routes.push(route(MIC, OBS));
        assert_eq!(plan.validate(), Err(PlanError::DuplicateRoute { from: MIC, to: OBS }));
    }

    #[test]
    fn rejects_loops_even_through_disabled_routes() {
        let mut plan = sample_plan();
        let second_bus = NodeId(6);
        plan.nodes.push(node(second_bus, NodeKind::Bus));
        plan.routes.push(route(BUS, second_bus));
        plan.routes.push(PlanRoute {
            enabled: false,
            ..route(second_bus, BUS)
        });
        assert_eq!(plan.validate(), Err(PlanError::Loop(BUS)));
    }

    #[test]
    fn rejects_too_many_nodes() {
        let mut plan = sample_plan();
        let first_extra = 100;
        for id in first_extra..first_extra + MAX_NODES as u32 {
            plan.nodes.push(node(NodeId(id), NodeKind::Bus));
        }
        assert_eq!(plan.validate(), Err(PlanError::TooManyNodes(plan.nodes.len())));
    }

    #[test]
    fn unknown_fields_are_rejected_when_deserializing() {
        let json = r#"{"id":1,"kind":{"type":"bus"},"strip":{"gain":0,"muted":false},"extra":true}"#;
        assert!(serde_json::from_str::<PlanNode>(json).is_err());
        let kind_with_extra_field = r#"{"type":"virtual_input","slot":{"direction":"in","number":1},"extra":1}"#;
        assert!(serde_json::from_str::<NodeKind>(kind_with_extra_field).is_err());
    }

    #[test]
    fn plan_round_trips_through_json() {
        let plan = sample_plan();
        let json = serde_json::to_string(&plan).unwrap();
        assert_eq!(serde_json::from_str::<RoutePlan>(&json).unwrap(), plan);
    }

    #[test]
    fn node_kind_has_a_stable_wire_format() {
        let kind = NodeKind::VirtualOutput {
            slot: slot(SlotDirection::Out, 3),
        };
        assert_eq!(
            serde_json::to_string(&kind).unwrap(),
            r#"{"type":"virtual_output","slot":{"direction":"out","number":3}}"#
        );
    }
}
