//! 发给界面的状态快照（字段名为 camelCase，与 `app/src/api/types.ts` 一一对应）。
//!
//! 后端是唯一的状态来源：每次编辑后、以及引擎或槽位状态变化后，都把完整快照推给界面（`state-changed` 事件）。
//! 快照只有几 KB，整体替换比增量同步简单可靠。

use aemeath_control::engine_client::DisconnectReason;
use aemeath_control::slots::{SlotHealth, SlotStatus};
use aemeath_core::model::{Node, NodeKind, Route};
use aemeath_core::preferences::LanguagePreference;
use aemeath_protocol::ids::{SlotDirection, SlotId};
use aemeath_protocol::message::{DeviceFormat, EndpointStatus, OfflineReason, SampleFormat};
use aemeath_protocol::topology::RouteRejection;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewState {
    pub engine: EngineView,
    /// 首次注册的进度；已注册时为 None
    pub registration: Option<RegistrationView>,
    /// 还没有配置文件：界面需要用本地化的名称创建默认输入（ADR-0009）
    pub first_run: bool,
    pub can_undo: bool,
    pub usbip_version: Option<String>,
    pub language: LanguagePreference,
    pub nodes: Vec<NodeView>,
    pub routes: Vec<RouteView>,
    pub slots: Vec<SlotView>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum EngineState {
    /// 还没连上（引擎正在启动，或者没有运行）
    Connecting,
    Running,
    /// 心跳超时，等待守护进程重启引擎
    Unresponsive,
    /// 引擎与主程序版本不配套
    VersionMismatch,
}

impl EngineState {
    pub fn after_disconnect(reason: &DisconnectReason) -> Self {
        match reason {
            DisconnectReason::Unresponsive => Self::Unresponsive,
            DisconnectReason::VersionMismatch { .. } => Self::VersionMismatch,
            DisconnectReason::Closed | DisconnectReason::HandshakeTimeout | DisconnectReason::ProtocolError(_) => {
                Self::Connecting
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineView {
    pub state: EngineState,
    pub sample_rate: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationView {
    pub completed: usize,
    pub total: usize,
    /// 多次修复失败的槽位
    pub failing: Vec<SlotRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotRef {
    pub direction: Direction,
    pub number: u8,
}

impl From<SlotId> for SlotRef {
    fn from(slot: SlotId) -> Self {
        Self {
            direction: match slot.direction {
                SlotDirection::In => Direction::In,
                SlotDirection::Out => Direction::Out,
            },
            number: slot.number.get(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Direction {
    In,
    Out,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NodeKindView {
    ExternalInput,
    VirtualInput,
    VirtualOutput,
    ExternalOutput,
    Bus,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceView {
    pub endpoint_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeView {
    pub id: u32,
    pub name: String,
    pub kind: NodeKindView,
    pub slot: Option<SlotRef>,
    pub device: Option<DeviceView>,
    pub x: f64,
    pub y: f64,
    pub gain_db: f32,
    pub muted: bool,
    pub status: NodeStatus,
}

/// 节点的运行状态（工作台上的离线提示、标题栏的离线节点数）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum NodeStatus {
    /// 在线、正常收发
    Ok,
    /// 正在准备（引擎正在连接、槽位等待 Windows 让端点出现）
    Pending,
    Offline {
        reason: OfflineReasonView,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum OfflineReasonView {
    WaitingForDevice,
    InUseByOtherApp,
    NoSignal,
    /// 槽位正在重新挂载
    SlotRepairing,
    /// 槽位多次挂载失败
    SlotFailing,
}

impl NodeStatus {
    pub fn of_endpoint(status: Option<&EndpointStatus>) -> Self {
        match status {
            Some(EndpointStatus::Online { .. }) => Self::Ok,
            Some(EndpointStatus::Offline { reason }) => Self::Offline {
                reason: match reason {
                    OfflineReason::WaitingForDevice => OfflineReasonView::WaitingForDevice,
                    OfflineReason::InUseByOtherApp => OfflineReasonView::InUseByOtherApp,
                    OfflineReason::NoSignal => OfflineReasonView::NoSignal,
                },
            },
            None => Self::Pending,
        }
    }

    pub fn of_slot(health: Option<SlotHealth>) -> Self {
        match health {
            Some(SlotHealth::Healthy) => Self::Ok,
            Some(SlotHealth::Retrying { .. }) => Self::Offline {
                reason: OfflineReasonView::SlotRepairing,
            },
            Some(SlotHealth::Failing { .. }) => Self::Offline {
                reason: OfflineReasonView::SlotFailing,
            },
            Some(SlotHealth::Unplugged | SlotHealth::EngineOffline | SlotHealth::Settling) | None => Self::Pending,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteView {
    pub from: u32,
    pub to: u32,
    pub gain_db: f32,
    pub enabled: bool,
    pub label_position: f32,
}

impl From<&Route> for RouteView {
    fn from(route: &Route) -> Self {
        Self {
            from: route.from.0,
            to: route.to.0,
            gain_db: route.gain.db(),
            enabled: route.enabled,
            label_position: route.label_position.get(),
        }
    }
}

/// 设备页的一行。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotView {
    pub slot: SlotRef,
    /// Windows 显示的设备名（找不到端点时为 None）
    pub device_name: Option<String>,
    pub occupant: Option<u32>,
    pub health: SlotHealthView,
    pub format: Option<FormatView>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum SlotHealthView {
    Unplugged,
    EngineOffline,
    Settling,
    Healthy,
    Retrying { failures: u32 },
    Failing { failures: u32 },
}

impl From<SlotHealth> for SlotHealthView {
    fn from(health: SlotHealth) -> Self {
        match health {
            SlotHealth::Unplugged => Self::Unplugged,
            SlotHealth::EngineOffline => Self::EngineOffline,
            SlotHealth::Settling => Self::Settling,
            SlotHealth::Healthy => Self::Healthy,
            SlotHealth::Retrying { failures, .. } => Self::Retrying { failures },
            SlotHealth::Failing { failures, .. } => Self::Failing { failures },
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FormatView {
    pub sample_rate: u32,
    pub sample_format: SampleFormatView,
    pub channels: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SampleFormatView {
    Int16,
    Int24,
    Int24In32,
    Int32,
    Float32,
    Float64,
}

impl From<DeviceFormat> for FormatView {
    fn from(format: DeviceFormat) -> Self {
        Self {
            sample_rate: format.sample_rate.hz(),
            sample_format: match format.sample_format {
                SampleFormat::Int16 => SampleFormatView::Int16,
                SampleFormat::Int24 => SampleFormatView::Int24,
                SampleFormat::Int24In32 => SampleFormatView::Int24In32,
                SampleFormat::Int32 => SampleFormatView::Int32,
                SampleFormat::Float32 => SampleFormatView::Float32,
                SampleFormat::Float64 => SampleFormatView::Float64,
            },
            channels: format.channels,
        }
    }
}

impl SlotView {
    pub fn new(slot: SlotId, status: Option<&SlotStatus>, occupant: Option<&Node>) -> Self {
        let endpoint = status.and_then(|status| status.endpoint.as_ref());
        Self {
            slot: slot.into(),
            device_name: endpoint.and_then(|endpoint| endpoint.name.clone()),
            occupant: occupant.map(|node| node.id.0),
            health: status.map_or(SlotHealthView::Unplugged, |status| status.health.into()),
            format: endpoint.and_then(|endpoint| endpoint.format).map(FormatView::from),
        }
    }
}

pub fn kind_view(kind: &NodeKind) -> NodeKindView {
    match kind {
        NodeKind::ExternalInput { .. } => NodeKindView::ExternalInput,
        NodeKind::VirtualInput { .. } => NodeKindView::VirtualInput,
        NodeKind::VirtualOutput { .. } => NodeKindView::VirtualOutput,
        NodeKind::ExternalOutput { .. } => NodeKindView::ExternalOutput,
        NodeKind::Bus => NodeKindView::Bus,
    }
}

/// 拖线时某个候选节点能否连上；不能时给出原因（界面据此显示提示）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteVerdict {
    pub node: u32,
    pub rejection: Option<RejectionView>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RejectionView {
    SelfRoute,
    CannotSend,
    CannotReceive,
    Duplicate,
    Loop,
}

impl From<RouteRejection> for RejectionView {
    fn from(rejection: RouteRejection) -> Self {
        match rejection {
            RouteRejection::SelfRoute => Self::SelfRoute,
            RouteRejection::CannotSend => Self::CannotSend,
            RouteRejection::CannotReceive => Self::CannotReceive,
            RouteRejection::Duplicate => Self::Duplicate,
            RouteRejection::Loop => Self::Loop,
        }
    }
}
