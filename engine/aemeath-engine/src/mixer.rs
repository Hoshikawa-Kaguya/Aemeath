//! 执行路由拓扑：通道条增益与静音、连线增益与开关、求和（选型报告 6.2 节）。
//!
//! 构建在控制线程进行（会分配内存）；[`Mixer::process`] 在调度线程按块运行，不分配内存、不加锁。
//!
//! 换拓扑时不能让声音跳变：
//! - 控制线程构建新混音器时，记下每个节点、每条连线对应旧拓扑中的位置；调度线程替换时调用
//!   [`Mixer::inherit`]，按位置接过旧的当前增益，再平滑过渡到新的目标值。
//! - 新连线从 0 淡入；被删除的连线如果两端节点还在、且不会和新连线构成回路，就保留下来淡出。
//! - 外部设备节点的端口（与设备 IO 线程之间的环形缓冲）按"端点 + 方向"从旧混音器移交过来，设备流不中断。

use std::collections::HashMap;
use std::sync::Arc;

use aemeath_dsp::gain::{SmoothedGain, db_to_linear};
use aemeath_protocol::graph;
use aemeath_protocol::ids::{EndpointId, NodeId, SlotId, TopologyRevision};
use aemeath_protocol::topology::{NodeKind, NodeRole, RouteTopology};
use aemeath_protocol::units::SampleRate;

use crate::STEREO;
use crate::meters::MeterBank;
use crate::port::{Port, PortIssues};

/// 一个块最多的帧数：1 ms 一块，768 kHz 时为 768 帧
pub const MAX_BLOCK_FRAMES: usize = 1024;
/// 增益变化的过渡时间
const RAMP_SECONDS: f64 = 0.010;

#[derive(Debug)]
struct MixNode {
    /// 节点类型：角色、槽位、外部端点都从这里取
    kind: NodeKind,
    strip: SmoothedGain,
    strip_target: f32,
    /// 这个节点本块的信号（通道条之后），交错立体声
    buffer: Vec<f32>,
    /// 外部设备节点的端口；没有端口时外部输入为静音、外部输出被丢弃
    port: Option<Port>,
}

#[derive(Debug)]
struct MixRoute {
    from: usize,
    to: usize,
    gain: SmoothedGain,
    target: f32,
}

/// 上一份拓扑的布局：构建下一份时用来找对应关系。只在控制线程保存。
#[derive(Debug, Clone, Default)]
pub struct MixerLayout {
    node_ids: Vec<NodeId>,
    /// 拓扑中的连线（不含正在淡出的旧连线）
    routes: Vec<(NodeId, NodeId)>,
    /// 每个节点的外部端点（端点 + 角色），用来移交端口
    endpoints: Vec<Option<(EndpointId, NodeRole)>>,
}

#[derive(Debug)]
pub struct Mixer {
    revision: TopologyRevision,
    sample_rate: SampleRate,
    nodes: Vec<MixNode>,
    /// 拓扑顺序：信号源先于目的地处理
    order: Vec<usize>,
    /// 每个节点的输入连线
    incoming: Vec<Vec<usize>>,
    routes: Vec<MixRoute>,
    ramp_frames: u32,
    meters: Arc<MeterBank>,
    inherit_nodes: Vec<Option<usize>>,
    inherit_routes: Vec<Option<usize>>,
    inherit_ports: Vec<Option<usize>>,
    slot_inputs: Vec<(usize, SlotId)>,
    slot_outputs: Vec<(usize, SlotId)>,
}

impl Mixer {
    /// 从已通过校验的拓扑构建混音器。`previous` 是上一份拓扑的布局（第一份拓扑为 None）。
    pub fn build(
        topology: &RouteTopology,
        previous: Option<&MixerLayout>,
        meters: Arc<MeterBank>,
    ) -> (Self, MixerLayout) {
        let ramp_frames = (f64::from(topology.engine_sample_rate.hz()) * RAMP_SECONDS).round() as u32;
        let index_of: HashMap<NodeId, usize> = topology
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.id, index))
            .collect();

        let nodes: Vec<MixNode> = topology
            .nodes
            .iter()
            .map(|node| {
                let strip_target = if node.strip.muted {
                    0.0
                } else {
                    db_to_linear(node.strip.gain.db())
                };
                MixNode {
                    kind: node.kind.clone(),
                    strip: SmoothedGain::fixed(strip_target),
                    strip_target,
                    buffer: vec![0.0; MAX_BLOCK_FRAMES * STEREO],
                    port: None,
                }
            })
            .collect();

        let mut routes: Vec<MixRoute> = topology
            .routes
            .iter()
            .filter_map(|route| {
                let target = if route.enabled {
                    db_to_linear(route.gain.db())
                } else {
                    0.0
                };
                let mut gain = SmoothedGain::fixed(0.0);
                gain.set_target(target, ramp_frames);
                Some(MixRoute {
                    from: *index_of.get(&route.from)?,
                    to: *index_of.get(&route.to)?,
                    gain,
                    target,
                })
            })
            .collect();

        // 被删除的旧连线：两端还在、方向合法、加上它不会构成回路时保留，淡出到 0
        let current_routes: Vec<(NodeId, NodeId)> =
            topology.routes.iter().map(|route| (route.from, route.to)).collect();
        let mut edges: Vec<(usize, usize)> = routes.iter().map(|route| (route.from, route.to)).collect();
        if let Some(previous) = previous {
            for key in &previous.routes {
                if current_routes.contains(key) {
                    continue;
                }
                let (Some(&from), Some(&to)) = (index_of.get(&key.0), index_of.get(&key.1)) else {
                    continue;
                };
                let valid_direction = nodes[from].kind.can_send() && nodes[to].kind.can_receive();
                if valid_direction && !graph::reaches(&edges, to, from) {
                    edges.push((from, to));
                    routes.push(MixRoute {
                        from,
                        to,
                        gain: SmoothedGain::fixed(0.0),
                        target: 0.0,
                    });
                }
            }
        }
        // 拓扑已通过校验，不会有环；万一有，环上的节点不在结果里，等于静音
        let node_indices: Vec<usize> = (0..nodes.len()).collect();
        let order = graph::topological_order(&node_indices, &edges);

        let mut incoming = vec![Vec::new(); nodes.len()];
        for (index, route) in routes.iter().enumerate() {
            incoming[route.to].push(index);
        }

        let inherit_nodes = topology
            .nodes
            .iter()
            .map(|node| previous?.node_ids.iter().position(|id| *id == node.id))
            .collect();
        let route_keys: Vec<(NodeId, NodeId)> = routes
            .iter()
            .map(|route| (topology.nodes[route.from].id, topology.nodes[route.to].id))
            .collect();
        let inherit_routes = route_keys
            .iter()
            .map(|key| previous?.routes.iter().position(|previous_key| previous_key == key))
            .collect();

        let endpoints: Vec<Option<(EndpointId, NodeRole)>> = nodes
            .iter()
            .map(|node| node.kind.device().map(|endpoint| (endpoint.clone(), node.kind.role())))
            .collect();
        let inherit_ports = endpoints
            .iter()
            .map(|key| {
                let key = key.as_ref()?;
                previous?
                    .endpoints
                    .iter()
                    .position(|previous| previous.as_ref() == Some(key))
            })
            .collect();

        let slot_inputs = slot_nodes(&nodes, NodeRole::Input);
        let slot_outputs = slot_nodes(&nodes, NodeRole::Output);
        let layout = MixerLayout {
            node_ids: topology.nodes.iter().map(|node| node.id).collect(),
            routes: current_routes,
            endpoints,
        };
        let mixer = Self {
            revision: topology.revision,
            sample_rate: topology.engine_sample_rate,
            nodes,
            order,
            incoming,
            routes,
            ramp_frames,
            meters,
            inherit_nodes,
            inherit_routes,
            inherit_ports,
            slot_inputs,
            slot_outputs,
        };
        (mixer, layout)
    }

    /// 调度线程替换拓扑时调用：接过旧混音器的当前增益（再平滑过渡到新的目标值）和外部设备端口。不分配内存。
    ///
    /// 控制线程已经为某个节点接上新端口时（例如设备流重建），不再从旧混音器移交。
    pub fn inherit(&mut self, old: &mut Self) {
        for (node, source) in self.nodes.iter_mut().zip(&self.inherit_ports) {
            if node.port.is_none()
                && let Some(old_node) = source.and_then(|index| old.nodes.get_mut(index))
            {
                node.port = old_node.port.take();
            }
        }
        for (node, source) in self.nodes.iter_mut().zip(&self.inherit_nodes) {
            if let Some(old_node) = source.and_then(|index| old.nodes.get(index)) {
                let mut strip = old_node.strip;
                strip.set_target(node.strip_target, self.ramp_frames);
                node.strip = strip;
            }
        }
        for (route, source) in self.routes.iter_mut().zip(&self.inherit_routes) {
            if let Some(old_route) = source.and_then(|index| old.routes.get(index)) {
                let mut gain = old_route.gain;
                gain.set_target(route.target, self.ramp_frames);
                route.gain = gain;
            }
        }
    }

    /// 外部设备节点：(节点下标, 端点, 角色)
    pub fn external_nodes(&self) -> Vec<(usize, EndpointId, NodeRole)> {
        self.nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| Some((index, node.kind.device()?.clone(), node.kind.role())))
            .collect()
    }

    /// 这个节点替换时会从旧混音器接过端口
    pub fn inherits_port(&self, node: usize) -> bool {
        self.inherit_ports.get(node).is_some_and(Option::is_some)
    }

    /// 控制线程为外部设备节点接上新端口。
    pub fn attach_port(&mut self, node: usize, port: Port) {
        if let Some(node) = self.nodes.get_mut(node) {
            node.port = Some(port);
        }
    }

    /// 把外部输入端口的数据读进对应节点的缓冲（在 [`Self::begin_block`] 之后调用）。
    pub fn pull_ports(&mut self, frames: usize, issues: &mut PortIssues) {
        let len = frames.min(MAX_BLOCK_FRAMES) * STEREO;
        for node in self.nodes.iter_mut().filter(|node| node.kind.role() == NodeRole::Input) {
            if let Some(port) = node.port.as_mut() {
                port.pull(&mut node.buffer[..len], issues);
            }
        }
    }

    /// 把外部输出节点处理后的信号写进端口（在 [`Self::process`] 之后调用）。
    pub fn push_ports(&mut self, frames: usize, issues: &mut PortIssues) {
        let len = frames.min(MAX_BLOCK_FRAMES) * STEREO;
        for node in self
            .nodes
            .iter_mut()
            .filter(|node| node.kind.role() == NodeRole::Output)
        {
            if let Some(port) = node.port.as_mut() {
                port.push(&node.buffer[..len], issues);
            }
        }
    }

    pub fn revision(&self) -> TopologyRevision {
        self.revision
    }

    pub fn sample_rate(&self) -> SampleRate {
        self.sample_rate
    }

    /// 由虚拟输入槽位供给声音的节点（节点下标，槽位）
    pub fn slot_inputs(&self) -> &[(usize, SlotId)] {
        &self.slot_inputs
    }

    /// 把声音送往虚拟输出槽位的节点（节点下标，槽位）
    pub fn slot_outputs(&self) -> &[(usize, SlotId)] {
        &self.slot_outputs
    }

    /// 开始一个块：把所有输入节点清零。之后由调用方填入有声音的输入。
    pub fn begin_block(&mut self, frames: usize) {
        let len = frames.min(MAX_BLOCK_FRAMES) * STEREO;
        for node in self.nodes.iter_mut().filter(|node| node.kind.role() == NodeRole::Input) {
            node.buffer[..len].fill(0.0);
        }
    }

    /// 输入节点本块的缓冲，由调用方写入交错立体声。
    pub fn input_mut(&mut self, node: usize, frames: usize) -> Option<&mut [f32]> {
        let len = frames.min(MAX_BLOCK_FRAMES) * STEREO;
        self.nodes.get_mut(node).map(|node| &mut node.buffer[..len])
    }

    /// 节点本块处理后的信号。
    pub fn output(&self, node: usize, frames: usize) -> Option<&[f32]> {
        let len = frames.min(MAX_BLOCK_FRAMES) * STEREO;
        self.nodes.get(node).map(|node| &node.buffer[..len])
    }

    /// 处理一个块。输入节点的缓冲须已填好；其余节点按拓扑顺序求和、过通道条。
    pub fn process(&mut self, frames: usize) {
        let frames = frames.min(MAX_BLOCK_FRAMES);
        let len = frames * STEREO;
        for position in 0..self.order.len() {
            let index = self.order[position];
            // 取出目标缓冲，以便同时读取信号源的缓冲；mem::take 换入的是空 Vec，不分配内存
            let mut buffer = std::mem::take(&mut self.nodes[index].buffer);
            if self.nodes[index].kind.role() != NodeRole::Input {
                buffer[..len].fill(0.0);
                for &route_index in &self.incoming[index] {
                    let route = &mut self.routes[route_index];
                    if route.gain.is_silent() {
                        continue;
                    }
                    let source = &self.nodes[route.from].buffer;
                    for (out, input) in buffer[..len]
                        .as_chunks_mut::<STEREO>()
                        .0
                        .iter_mut()
                        .zip(source[..len].as_chunks::<STEREO>().0)
                    {
                        let gain = route.gain.next_gain();
                        out[0] += input[0] * gain;
                        out[1] += input[1] * gain;
                    }
                }
            }

            let node = &mut self.nodes[index];
            let mut peaks = [0f32; STEREO];
            for frame in buffer[..len].as_chunks_mut::<STEREO>().0.iter_mut() {
                let gain = node.strip.next_gain();
                for (sample, peak) in frame.iter_mut().zip(peaks.iter_mut()) {
                    *sample *= gain;
                    *peak = peak.max(sample.abs());
                }
            }
            node.buffer = buffer;
            self.meters.record(index, peaks);
        }
    }
}

fn slot_nodes(nodes: &[MixNode], role: NodeRole) -> Vec<(usize, SlotId)> {
    nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.kind.role() == role)
        .filter_map(|(index, node)| Some((index, node.kind.slot()?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aemeath_protocol::ids::{SlotDirection, SlotNumber};
    use aemeath_protocol::topology::{Resampler, Strip, TopologyNode, TopologyRoute};
    use aemeath_protocol::units::GainDb;

    const GAME: NodeId = NodeId(1);
    const MIC: NodeId = NodeId(2);
    const BUS: NodeId = NodeId(3);
    const OBS: NodeId = NodeId(4);
    const FRAMES: usize = 48;
    /// 48 kHz 下 10 ms 的过渡帧数
    const RAMP: usize = 480;

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

    fn route(from: NodeId, to: NodeId, gain_db: f32) -> TopologyRoute {
        TopologyRoute {
            from,
            to,
            gain: GainDb::new(gain_db).unwrap(),
            enabled: true,
        }
    }

    /// 游戏（In 1）和麦克风（In 2）→ 总线 → OBS（Out 1）
    fn topology(routes: Vec<TopologyRoute>) -> RouteTopology {
        RouteTopology {
            revision: TopologyRevision(1),
            engine_sample_rate: SampleRate::HZ_48000,
            clock_master: None,
            resampler: Resampler::default(),
            nodes: vec![
                node(
                    GAME,
                    NodeKind::VirtualInput {
                        slot: slot(SlotDirection::In, 1),
                    },
                ),
                node(
                    MIC,
                    NodeKind::VirtualInput {
                        slot: slot(SlotDirection::In, 2),
                    },
                ),
                node(BUS, NodeKind::Bus),
                node(
                    OBS,
                    NodeKind::VirtualOutput {
                        slot: slot(SlotDirection::Out, 1),
                    },
                ),
            ],
            routes,
        }
    }

    fn build(topology: &RouteTopology, previous: Option<&MixerLayout>) -> (Mixer, MixerLayout) {
        let meters = Arc::new(MeterBank::new(topology.nodes.iter().map(|node| node.id).collect()));
        Mixer::build(topology, previous, meters)
    }

    /// 输入节点填常数，运行若干块，返回 OBS 最后一帧左声道的值
    fn run(mixer: &mut Mixer, game: f32, mic: f32, blocks: usize) -> f32 {
        let mut last = 0.0;
        for _ in 0..blocks {
            mixer.begin_block(FRAMES);
            mixer.input_mut(0, FRAMES).unwrap().fill(game);
            mixer.input_mut(1, FRAMES).unwrap().fill(mic);
            mixer.process(FRAMES);
            last = mixer.output(3, FRAMES).unwrap()[FRAMES * STEREO - 2];
        }
        last
    }

    #[test]
    fn sums_routes_with_their_gains_after_the_ramp() {
        let topology = topology(vec![route(GAME, BUS, 0.0), route(MIC, BUS, -6.0), route(BUS, OBS, 0.0)]);
        let (mut mixer, _) = build(&topology, None);
        let settled = run(&mut mixer, 0.5, 0.5, RAMP / FRAMES + 1);
        assert!((settled - (0.5 + 0.5 * db_to_linear(-6.0))).abs() < 1e-5, "{settled}");
    }

    #[test]
    fn new_routes_fade_in_instead_of_jumping() {
        let topology = topology(vec![route(GAME, OBS, 0.0)]);
        let (mut mixer, _) = build(&topology, None);
        let first_block = run(&mut mixer, 1.0, 0.0, 1);
        // 第一块结束时只走了 48/480 的过渡
        assert!((first_block - 0.1).abs() < 1e-5, "{first_block}");
    }

    #[test]
    fn disabled_routes_and_muted_strips_are_silent() {
        let mut disabled = topology(vec![route(GAME, OBS, 0.0)]);
        disabled.routes[0].enabled = false;
        let (mut mixer, _) = build(&disabled, None);
        assert_eq!(run(&mut mixer, 1.0, 1.0, 20), 0.0);

        let mut muted = topology(vec![route(GAME, OBS, 0.0)]);
        muted.nodes[3].strip.muted = true;
        let (mut mixer, _) = build(&muted, None);
        assert_eq!(run(&mut mixer, 1.0, 1.0, 20), 0.0);
    }

    #[test]
    fn replacing_the_topology_continues_from_the_current_gain() {
        let first = topology(vec![route(GAME, OBS, 0.0)]);
        let (mut old, layout) = build(&first, None);
        assert!((run(&mut old, 1.0, 0.0, 20) - 1.0).abs() < 1e-6);

        let mut second = topology(vec![route(GAME, OBS, -6.0)]);
        second.revision = TopologyRevision(2);
        let (mut new, _) = build(&second, Some(&layout));
        new.inherit(&mut old);
        // 从 1.0 出发过渡到 0.5，第一块结束时只下降了十分之一
        let first_block = run(&mut new, 1.0, 0.0, 1);
        let expected = 1.0 + (db_to_linear(-6.0) - 1.0) * 0.1;
        assert!((first_block - expected).abs() < 1e-5, "{first_block}");
    }

    #[test]
    fn removed_routes_fade_out() {
        let first = topology(vec![route(GAME, OBS, 0.0)]);
        let (mut old, layout) = build(&first, None);
        run(&mut old, 1.0, 0.0, 20);

        let second = topology(vec![]);
        let (mut new, _) = build(&second, Some(&layout));
        new.inherit(&mut old);
        let first_block = run(&mut new, 1.0, 0.0, 1);
        assert!((first_block - 0.9).abs() < 1e-5, "{first_block}");
        assert_eq!(run(&mut new, 1.0, 0.0, 20), 0.0);
    }

    #[test]
    fn removed_routes_between_remaining_nodes_are_kept_to_fade_out() {
        let first = topology(vec![route(GAME, BUS, 0.0), route(BUS, OBS, 0.0)]);
        let (_, layout) = build(&first, None);
        // 新拓扑去掉 BUS→OBS，两端节点都还在，方向也与新的拓扑顺序一致
        let second = topology(vec![route(GAME, BUS, 0.0), route(MIC, OBS, 0.0)]);
        let (new, _) = build(&second, Some(&layout));
        assert_eq!(new.routes.len(), 3, "BUS→OBS is kept to fade out");
        assert_eq!(new.order.len(), 4);
    }

    #[test]
    fn removed_routes_that_would_form_a_loop_are_dropped() {
        let second_bus = NodeId(5);
        let with_two_buses = |routes| {
            let mut topology = topology(routes);
            topology.nodes.push(node(second_bus, NodeKind::Bus));
            topology
        };
        let (_, layout) = build(&with_two_buses(vec![route(BUS, second_bus, 0.0)]), None);
        let (new, _) = build(&with_two_buses(vec![route(second_bus, BUS, 0.0)]), Some(&layout));
        assert_eq!(new.routes.len(), 1, "BUS→second bus would close a loop");
        assert_eq!(new.order.len(), 5);
    }

    #[test]
    fn external_ports_move_to_the_new_mixer_unless_replaced() {
        let microphone = NodeId(7);
        let with_microphone = |revision| {
            let mut topology = topology(vec![route(microphone, OBS, 0.0)]);
            topology.revision = TopologyRevision(revision);
            topology.nodes.push(node(
                microphone,
                NodeKind::ExternalInput {
                    device: EndpointId("{0.0.1.00000000}.{mic}".to_owned()),
                },
            ));
            topology
        };
        let (mut first, layout) = build(&with_microphone(1), None);
        let (_, consumer) = rtrb::RingBuffer::<f32>::new(16);
        first.attach_port(4, Port::input(consumer, 4));

        let (mut second, layout) = build(&with_microphone(2), Some(&layout));
        assert!(second.inherits_port(4));
        second.inherit(&mut first);
        assert!(second.nodes[4].port.is_some() && first.nodes[4].port.is_none());

        // 控制线程已经接了新端口（设备流重建）：不再从旧混音器接
        let (mut third, _) = build(&with_microphone(3), Some(&layout));
        let (_, fresh) = rtrb::RingBuffer::<f32>::new(16);
        third.attach_port(4, Port::input(fresh, 4));
        third.inherit(&mut second);
        assert!(
            second.nodes[4].port.is_some(),
            "the old port stays behind and is dropped with the old mixer"
        );
    }

    #[test]
    fn reports_slot_nodes_and_meters() {
        let topology = topology(vec![route(GAME, OBS, 0.0)]);
        let (mut mixer, _) = build(&topology, None);
        assert_eq!(mixer.slot_inputs().len(), 2);
        assert_eq!(mixer.slot_outputs(), &[(3, slot(SlotDirection::Out, 1))]);
        run(&mut mixer, 0.25, 0.0, 1);
        assert_eq!(mixer.meters.take(0), Some([0.25, 0.25]));
    }
}
