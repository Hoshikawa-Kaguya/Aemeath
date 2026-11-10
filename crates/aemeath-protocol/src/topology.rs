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
use crate::ids::{EndpointId, NodeId, ProgramPath, SlotDirection, SlotId, TopologyRevision};
use crate::message::{DeviceFormat, SampleFormat};
use crate::units::{BufferMs, GainDb, GateThreshold, SampleRate};

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
    /// 全局默认的重采样算法；节点可以单独覆盖
    pub resampler: Resampler,
    pub nodes: Vec<TopologyNode>,
    pub routes: Vec<TopologyRoute>,
}

/// 重采样算法及其重采样预设（GLOSSARY）。参数不对用户开放，每个预设对应一组固定参数（`aemeath-dsp`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "algorithm", rename_all = "snake_case", deny_unknown_fields)]
pub enum Resampler {
    PolyAsync { preset: PolyPreset },
    SincAsync { preset: SincPreset },
    FftSync { preset: FftPreset },
}

impl Default for Resampler {
    fn default() -> Self {
        Self::SincAsync {
            preset: SincPreset::default(),
        }
    }
}

/// Poly Async 的重采样预设：多项式次数依次升高。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolyPreset {
    Fastest,
    Smooth,
    #[default]
    Fine,
}

/// Sinc Async 的重采样预设：滤波器依次变长，通带更宽，延迟和 CPU 开销更高。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SincPreset {
    VeryFast,
    Fast,
    #[default]
    Balanced,
    Accurate,
    Ultra,
}

/// FFT Sync 的重采样预设：分块越大延迟越高，通带越宽。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FftPreset {
    #[default]
    LowLatency,
    Balanced,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TopologyNode {
    pub id: NodeId,
    pub kind: NodeKind,
    pub strip: Strip,
    /// 外部设备节点的打开方式与重采样算法；只有外部设备节点可以有。外部设备节点没有时按默认值（独占）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub io: Option<DeviceSettings>,
    /// 占用槽位的节点的格式与重采样算法（GLOSSARY：跟随引擎）；只有占用槽位的节点可以有
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot_settings: Option<SlotNodeSettings>,
}

/// 占用槽位的节点的设置。属于节点，不属于槽位：节点换槽位时带到新槽位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlotNodeSettings {
    #[serde(default)]
    pub format: SlotFormatChoice,
    /// 按节点覆盖的重采样算法；None 表示使用全局默认（[`RouteTopology::resampler`]）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resampler: Option<Resampler>,
}

/// 槽位设备格式的来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "choice", rename_all = "snake_case", deny_unknown_fields)]
pub enum SlotFormatChoice {
    /// 跟随引擎采样率，最高到 [`SLOT_FORMAT_MAX_HZ`]
    #[default]
    FollowEngine,
    /// 指定采样率和样本格式；靠固定比率重采样转换到引擎采样率
    Fixed {
        sample_rate: SampleRate,
        sample_format: SampleFormat,
    },
}

/// 槽位设备格式的采样率上限：共享模式下 Windows 不接受更高的采样率（M4 计划 E1）
pub const SLOT_FORMAT_MAX_HZ: u32 = 384_000;

impl SlotFormatChoice {
    pub fn is_valid(self) -> bool {
        match self {
            Self::FollowEngine => true,
            Self::Fixed {
                sample_rate,
                sample_format,
            } => sample_rate.hz() <= SLOT_FORMAT_MAX_HZ && sample_format != SampleFormat::Float64,
        }
    }
}

/// 外部设备节点的设备设置（ADR-0014）。
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceSettings {
    #[serde(default)]
    pub access: DeviceAccess,
    /// 按节点覆盖的重采样算法；None 表示使用全局默认（[`RouteTopology::resampler`]）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resampler: Option<Resampler>,
}

impl DeviceSettings {
    /// 实际使用的重采样算法
    pub fn resampler_or(&self, global: Resampler) -> Resampler {
        self.resampler.unwrap_or(global)
    }
}

/// 外部设备的打开方式（GLOSSARY：独占）。默认独占。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeviceAccess {
    /// Aemeath 独自使用端点；设备格式和缓冲由用户决定
    Exclusive {
        #[serde(default)]
        format: FormatChoice,
        #[serde(default)]
        buffer: BufferChoice,
    },
    /// 与其他程序共用端点；设备格式由 Windows 决定
    Shared,
}

impl Default for DeviceAccess {
    fn default() -> Self {
        Self::Exclusive {
            format: FormatChoice::default(),
            buffer: BufferChoice::default(),
        }
    }
}

impl DeviceAccess {
    pub fn is_exclusive(&self) -> bool {
        matches!(self, Self::Exclusive { .. })
    }
}

/// 独占时的设备格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "choice", rename_all = "snake_case", deny_unknown_fields)]
pub enum FormatChoice {
    /// 打开时按 [`crate::message::DeviceCapabilities::recommended`] 选定
    #[default]
    Recommended,
    /// 用户指定的格式；设备不再支持时退回推荐格式并记录警告
    Fixed { format: DeviceFormat },
}

/// 独占时的缓冲（设备周期）。
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(tag = "choice", rename_all = "snake_case", deny_unknown_fields)]
pub enum BufferChoice {
    /// 设备的默认周期（通常 10 ms）
    #[default]
    DeviceDefault,
    /// 设备的最小周期
    DeviceMinimum,
    /// 自定义；不低于设备的最小周期，并按设备要求对齐
    Milliseconds { ms: BufferMs },
}

/// 节点类型。
///
/// `D` 是外部设备的表示：路由拓扑里只是端点 ID；主程序的工作台里还带上设备名，用于后备匹配。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum NodeKind<D = EndpointId> {
    /// 外部输入（A）：不专属于 Aemeath 的录音端点
    ExternalInput { device: D },
    /// 应用源：按路径认定的程序，进程回环采集（GLOSSARY：应用源）
    AppSource {
        program: ProgramPath,
        /// 按节点覆盖的重采样算法；None 表示使用全局默认
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resampler: Option<Resampler>,
    },
    /// 虚拟输入（B）：占用一个输入方向的槽位
    VirtualInput { slot: SlotId },
    /// 虚拟输出（C）：占用一个输出方向的槽位
    VirtualOutput { slot: SlotId },
    /// 外部输出（D）：不专属于 Aemeath 的播放端点
    ExternalOutput { device: D },
    /// 总线：只求和再分发
    Bus,
    /// 外部插入（GLOSSARY：外部插入）：入边的求和从送出端（输出方向的槽位）交给外部程序，
    /// 节点发出的信号从取回端（一个外部输入）读回来
    ExternalInsert { send: SlotId, ret: D },
    /// 录音节点：把收到的信号写进文件（M3 计划 2.5）
    Recorder { settings: RecorderSettings },
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
            Self::ExternalInput { .. } | Self::VirtualInput { .. } | Self::AppSource { .. } => NodeRole::Input,
            Self::Bus | Self::ExternalInsert { .. } => NodeRole::Processing,
            Self::VirtualOutput { .. } | Self::ExternalOutput { .. } | Self::Recorder { .. } => NodeRole::Output,
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
            Self::ExternalInsert { send, .. } => Some(*send),
            _ => None,
        }
    }

    /// 占用槽位的节点类型要求的槽位方向
    pub fn slot_direction(&self) -> Option<SlotDirection> {
        match self {
            Self::VirtualInput { .. } => Some(SlotDirection::In),
            Self::VirtualOutput { .. } | Self::ExternalInsert { .. } => Some(SlotDirection::Out),
            _ => None,
        }
    }

    /// 节点使用的外部设备（外部插入是它的取回端）
    pub fn device(&self) -> Option<&D> {
        match self {
            Self::ExternalInput { device } | Self::ExternalOutput { device } => Some(device),
            Self::ExternalInsert { ret, .. } => Some(ret),
            _ => None,
        }
    }

    /// 外部设备在 Aemeath 看来的方向：录音端点是 [`NodeRole::Input`]，播放端点是 [`NodeRole::Output`]。
    /// 外部插入的角色是处理节点，它的取回端却是录音端点，所以不能用 [`Self::role`] 判断设备方向。
    pub fn endpoint_role(&self) -> Option<NodeRole> {
        match self {
            Self::ExternalInput { .. } | Self::ExternalInsert { .. } => Some(NodeRole::Input),
            Self::ExternalOutput { .. } => Some(NodeRole::Output),
            _ => None,
        }
    }

    /// 能否作为主时钟。外部插入的取回端总是经过 ASRC（M3 计划 X8）。
    pub fn can_be_clock_master(&self) -> bool {
        matches!(self, Self::ExternalInput { .. } | Self::ExternalOutput { .. })
    }

    pub fn is_external(&self) -> bool {
        self.device().is_some()
    }

    /// 应用源节点的程序
    pub fn program(&self) -> Option<&ProgramPath> {
        match self {
            Self::AppSource { program, .. } => Some(program),
            _ => None,
        }
    }

    /// 换一种外部设备的表示（例如从工作台的设备记录取出端点 ID），其余不变。
    pub fn map_device<E>(&self, map: impl FnOnce(&D) -> E) -> NodeKind<E> {
        match self {
            Self::ExternalInput { device } => NodeKind::ExternalInput { device: map(device) },
            Self::AppSource { program, resampler } => NodeKind::AppSource {
                program: program.clone(),
                resampler: *resampler,
            },
            Self::VirtualInput { slot } => NodeKind::VirtualInput { slot: *slot },
            Self::VirtualOutput { slot } => NodeKind::VirtualOutput { slot: *slot },
            Self::ExternalOutput { device } => NodeKind::ExternalOutput { device: map(device) },
            Self::Bus => NodeKind::Bus,
            Self::ExternalInsert { send, ret } => NodeKind::ExternalInsert {
                send: *send,
                ret: map(ret),
            },
            Self::Recorder { settings } => NodeKind::Recorder { settings: *settings },
        }
    }
}

/// 录音文件的容器格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordFormat {
    Wav,
    #[default]
    Flac,
}

/// 录音文件的位深。FLAC 不支持浮点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordDepth {
    Int16,
    #[default]
    Int24,
    Float32,
}

/// 录音节点的格式设置，默认 FLAC 24 位（M3 计划 R2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecorderSettings {
    pub format: RecordFormat,
    pub depth: RecordDepth,
}

impl RecorderSettings {
    pub fn is_valid(self) -> bool {
        !(self.format == RecordFormat::Flac && self.depth == RecordDepth::Float32)
    }
}

/// 通道条参数（GLOSSARY：通道条）。输入节点的处理顺序是门限 → 增益 → 静音；Aemeath 不提供 Solo 和监听（ADR-0004）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Strip {
    pub gain: GainDb,
    pub muted: bool,
    /// 门限阈值；None 表示关。只有输入节点可以设门限
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<GateThreshold>,
}

impl Default for Strip {
    fn default() -> Self {
        Self {
            gain: GainDb::UNITY,
            muted: false,
            gate: None,
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
    #[error("{second} captures the same program as {first}")]
    DuplicateProgram { first: NodeId, second: NodeId },
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
    #[error("{0} records FLAC with floating-point samples, which FLAC does not support")]
    InvalidRecorderSettings(NodeId),
    #[error("{0} has a gate, but only input nodes have one")]
    GateOnNonInput(NodeId),
    #[error("{0} has device settings, but only external device nodes have them")]
    DeviceSettingsOnNonExternal(NodeId),
    #[error("{0} has a fixed device format without channels")]
    InvalidDeviceFormat(NodeId),
    #[error("{0} does not occupy a slot but has slot settings")]
    SlotSettingsOnNonSlot(NodeId),
    #[error("{0} has a slot format the virtual device cannot use")]
    InvalidSlotFormat(NodeId),
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
            && !nodes.get(&master).is_some_and(|kind| kind.can_be_clock_master())
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
        // 同一个程序只能有一个应用源，否则同一份声音会叠加（M3 计划 A6）
        let mut programs: HashMap<String, NodeId> = HashMap::new();
        for node in &self.nodes {
            if nodes.insert(node.id, &node.kind).is_some() {
                return Err(TopologyError::DuplicateNode(node.id));
            }
            if let NodeKind::Recorder { settings } = node.kind
                && !settings.is_valid()
            {
                return Err(TopologyError::InvalidRecorderSettings(node.id));
            }
            if let Some(program) = node.kind.program()
                && let Some(first) = programs.insert(program.as_str().to_lowercase(), node.id)
            {
                return Err(TopologyError::DuplicateProgram { first, second: node.id });
            }
            if node.strip.gate.is_some() && node.kind.role() != NodeRole::Input {
                return Err(TopologyError::GateOnNonInput(node.id));
            }
            if let Some(settings) = node.slot_settings {
                if node.kind.slot().is_none() {
                    return Err(TopologyError::SlotSettingsOnNonSlot(node.id));
                }
                if !settings.format.is_valid() {
                    return Err(TopologyError::InvalidSlotFormat(node.id));
                }
            }
            if let Some(io) = &node.io {
                if !node.kind.is_external() {
                    return Err(TopologyError::DeviceSettingsOnNonExternal(node.id));
                }
                if let DeviceAccess::Exclusive {
                    format: FormatChoice::Fixed { format },
                    ..
                } = io.access
                    && format.channels == 0
                {
                    return Err(TopologyError::InvalidDeviceFormat(node.id));
                }
            }
            if let (Some(endpoint), Some(role)) = (node.kind.device(), node.kind.endpoint_role()) {
                if !endpoint.is_well_formed() {
                    return Err(TopologyError::InvalidEndpoint { node: node.id });
                }
                if let Some(first) = endpoints.insert((endpoint, role), node.id) {
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
            slot_settings: None,
            io: None,
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
    fn only_input_nodes_have_a_gate() {
        let threshold = Some(GateThreshold::new(-40.0).unwrap());
        let mut topology = sample_topology();
        topology.nodes[0].strip.gate = threshold;
        assert_eq!(topology.validate(), Ok(()));
        topology.nodes[2].strip.gate = threshold;
        assert_eq!(topology.validate(), Err(TopologyError::GateOnNonInput(BUS)));
    }

    #[test]
    fn a_program_has_at_most_one_app_source_and_cannot_be_the_clock_master() {
        let source = |path: &str| NodeKind::AppSource {
            program: ProgramPath::new(path).unwrap(),
            resampler: None,
        };
        let (first, second) = (NodeId(20), NodeId(21));
        let mut topology = sample_topology();
        topology.nodes.push(node(first, source(r"C:\Apps\Player.exe")));
        assert_eq!(topology.validate(), Ok(()));
        topology.clock_master = Some(first);
        assert_eq!(topology.validate(), Err(TopologyError::InvalidClockMaster(first)));
        topology.clock_master = None;
        topology.nodes.push(node(second, source(r"c:\apps\player.EXE")));
        assert_eq!(
            topology.validate(),
            Err(TopologyError::DuplicateProgram { first, second })
        );
    }

    #[test]
    fn an_external_insert_shares_slot_and_device_rules_but_is_not_a_clock_master() {
        let insert_id = NodeId(30);
        let insert = |send, ret: &str| NodeKind::ExternalInsert {
            send,
            ret: EndpointId(ret.to_owned()),
        };
        let mut topology = sample_topology();
        topology
            .nodes
            .push(node(insert_id, insert(slot(SlotDirection::Out, 2), "{broadcast}")));
        topology.routes.push(route(MIC, insert_id));
        topology.routes.push(route(insert_id, HEADPHONES));
        assert_eq!(topology.validate(), Ok(()));

        let mut clock = topology.clone();
        clock.clock_master = Some(insert_id);
        assert_eq!(clock.validate(), Err(TopologyError::InvalidClockMaster(insert_id)));

        let mut taken = topology.clone();
        taken.nodes.last_mut().unwrap().kind = insert(slot(SlotDirection::Out, 1), "{broadcast}");
        assert!(matches!(taken.validate(), Err(TopologyError::SlotOccupiedTwice { .. })));

        let mut wrong_direction = topology.clone();
        wrong_direction.nodes.last_mut().unwrap().kind = insert(slot(SlotDirection::In, 2), "{broadcast}");
        assert!(matches!(
            wrong_direction.validate(),
            Err(TopologyError::SlotDirectionMismatch { .. })
        ));

        let mut same_device = topology.clone();
        same_device.nodes.last_mut().unwrap().kind = insert(slot(SlotDirection::Out, 2), "{0.0.1.00000000}.{mic}");
        assert_eq!(
            same_device.validate(),
            Err(TopologyError::DuplicateEndpoint {
                first: MIC,
                second: insert_id,
            })
        );

        let mut looped = topology;
        looped.routes.push(route(insert_id, BUS));
        looped.routes.push(route(BUS, insert_id));
        assert!(matches!(
            looped.validate(),
            Err(TopologyError::InvalidRoute {
                rejection: RouteRejection::Loop,
                ..
            })
        ));
    }

    #[test]
    fn flac_cannot_use_floating_point_samples() {
        let id = NodeId(40);
        let recorder = |format, depth| NodeKind::Recorder {
            settings: RecorderSettings { format, depth },
        };
        let mut topology = sample_topology();
        topology
            .nodes
            .push(node(id, recorder(RecordFormat::Wav, RecordDepth::Float32)));
        assert_eq!(topology.validate(), Ok(()));
        topology.nodes.last_mut().unwrap().kind = recorder(RecordFormat::Flac, RecordDepth::Float32);
        assert_eq!(topology.validate(), Err(TopologyError::InvalidRecorderSettings(id)));
    }

    #[test]
    fn app_sources_have_a_stable_wire_format() {
        let kind: NodeKind = NodeKind::AppSource {
            program: ProgramPath::new(r"C:\a.exe").unwrap(),
            resampler: None,
        };
        assert_eq!(
            serde_json::to_string(&kind).unwrap(),
            r#"{"type":"app_source","program":"C:\\a.exe"}"#
        );
    }

    #[test]
    fn device_settings_default_to_exclusive_and_only_fit_external_nodes() {
        let settings: DeviceSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings, DeviceSettings::default());
        assert!(settings.access.is_exclusive());
        assert_eq!(
            serde_json::to_string(&settings).unwrap(),
            r#"{"access":{"mode":"exclusive","format":{"choice":"recommended"},"buffer":{"choice":"device_default"}}}"#
        );
        let custom: DeviceSettings =
            serde_json::from_str(r#"{"access":{"mode":"exclusive","buffer":{"choice":"milliseconds","ms":3.5}}}"#)
                .unwrap();
        assert_eq!(
            custom.access,
            DeviceAccess::Exclusive {
                format: FormatChoice::Recommended,
                buffer: BufferChoice::Milliseconds {
                    ms: BufferMs::new(3.5).unwrap()
                },
            }
        );
        assert!(serde_json::from_str::<BufferChoice>(r#"{"choice":"milliseconds","ms":0.1}"#).is_err());

        let mut topology = sample_topology();
        topology.nodes[0].io = Some(DeviceSettings {
            access: DeviceAccess::Shared,
            resampler: Some(Resampler::PolyAsync {
                preset: PolyPreset::Fastest,
            }),
        });
        assert_eq!(topology.validate(), Ok(()));
        topology.nodes[2].io = Some(DeviceSettings::default());
        assert_eq!(
            topology.validate(),
            Err(TopologyError::DeviceSettingsOnNonExternal(BUS))
        );
    }

    #[test]
    fn slot_settings_need_a_slot_and_a_usable_format() {
        let mut topology = sample_topology();
        let fixed = |hz: u32, sample_format| SlotNodeSettings {
            format: SlotFormatChoice::Fixed {
                sample_rate: SampleRate::new(hz).unwrap(),
                sample_format,
            },
            resampler: None,
        };
        let slot_index = topology
            .nodes
            .iter()
            .position(|node| node.kind.slot().is_some())
            .unwrap();
        let slot_node = topology.nodes[slot_index].id;
        topology.nodes[slot_index].slot_settings = Some(fixed(384_000, SampleFormat::Int24));
        assert_eq!(topology.validate(), Ok(()));
        topology.nodes[slot_index].slot_settings = Some(fixed(768_000, SampleFormat::Int24));
        assert_eq!(topology.validate(), Err(TopologyError::InvalidSlotFormat(slot_node)));
        topology.nodes[slot_index].slot_settings = Some(fixed(48_000, SampleFormat::Float64));
        assert_eq!(topology.validate(), Err(TopologyError::InvalidSlotFormat(slot_node)));
        topology.nodes[slot_index].slot_settings = None;
        let other = topology
            .nodes
            .iter()
            .position(|node| node.kind.slot().is_none())
            .unwrap();
        let other_id = topology.nodes[other].id;
        topology.nodes[other].slot_settings = Some(SlotNodeSettings::default());
        assert_eq!(topology.validate(), Err(TopologyError::SlotSettingsOnNonSlot(other_id)));
    }

    #[test]
    fn strips_without_a_gate_keep_the_m1_wire_format() {
        assert_eq!(
            serde_json::to_string(&Strip::default()).unwrap(),
            r#"{"gain":0.0,"muted":false}"#
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
