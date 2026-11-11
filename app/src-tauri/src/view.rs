//! 发给界面的状态快照（字段名为 camelCase，与 `app/src/api/types.ts` 一一对应）。
//!
//! 后端是唯一的状态来源：每次编辑后、以及引擎或槽位状态变化后，都把完整快照推给界面（`state-changed` 事件）。
//! 快照只有几 KB，整体替换比增量同步简单可靠。

use aemeath_control::engine_client::DisconnectReason;
use aemeath_control::slots::{SlotHealth, SlotStatus};
use aemeath_core::model::{Node, NodeKind, Route};
use aemeath_core::preferences::{LanguagePreference, ThemeMode};
use aemeath_protocol::ids::{SlotDirection, SlotId};
use aemeath_protocol::message::{DeviceFormat, EndpointStatus, OfflineReason, SampleFormat};
use aemeath_protocol::topology::RouteRejection;
use aemeath_protocol::units::{SampleRate, SampleRateError};
use serde::{Deserialize, Serialize};

use crate::device_settings::{DeviceSettingsView, ResamplerView, SlotSettingsView};
use crate::hotkey_views::{HotkeyBindingView, HotkeyRowView};
use crate::recording::{RecorderView, RecordingFailureView, RecordingView};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewState {
    pub engine: EngineView,
    /// 首次注册的进度；已注册时为 None
    pub registration: Option<RegistrationView>,
    pub can_undo: bool,
    pub usbip_version: Option<String>,
    pub language: LanguagePreference,
    pub check_updates_on_start: bool,
    pub appearance: AppearanceView,
    pub last_page: Option<String>,
    pub nodes: Vec<NodeView>,
    pub routes: Vec<RouteView>,
    /// 固定到控制台的节点 ID，从左到右
    pub console: Vec<u32>,
    pub recording: RecordingView,
    pub hotkeys: Vec<HotkeyRowView>,
    /// 紧急静音开启中
    pub panic_mute: bool,
    pub slots: Vec<SlotView>,
    /// 最近的一次性提示（按编号递增），界面只显示没见过的
    pub notices: Vec<NoticeView>,
    /// 当前全部的重复路由（工作台上的文字警告）
    pub duplicates: Vec<DuplicateView>,
}

/// 后端主动给出的一次性提示。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoticeView {
    pub id: u64,
    pub kind: NoticeKind,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum NoticeKind {
    /// 录音出错，这一路已停止
    RecordingFailed { node: String, reason: RecordingFailureView },
    /// 外部设备换了端点 ID（例如换了 USB 口），已按名称自动改用
    DeviceReplaced { node: String, device: String },
    /// 程序的持久指派被别处改过，已放回 Aemeath 记录的落点
    LandingRestored { app: String, landing: String },
}

/// 一处重复路由的说明。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateView {
    /// 应用源的节点名
    pub program: String,
    /// 声音出现两份的目的地的节点名
    pub destination: String,
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
    /// 连接断开后显示的引擎状态：无响应和版本不符单独提示，其余都显示为正在连接。
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

/// 界面外观。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppearanceView {
    pub theme_mode: ThemeModeView,
    pub accent: Option<String>,
    /// 背景图的完整路径；没有设置或文件已丢失时为 None
    pub background_path: Option<String>,
    pub overlay_percent: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ThemeModeView {
    System,
    Dark,
    Light,
}

impl From<ThemeMode> for ThemeModeView {
    fn from(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::System => Self::System,
            ThemeMode::Dark => Self::Dark,
            ThemeMode::Light => Self::Light,
        }
    }
}

impl From<ThemeModeView> for ThemeMode {
    fn from(mode: ThemeModeView) -> Self {
        match mode {
            ThemeModeView::System => Self::System,
            ThemeModeView::Dark => Self::Dark,
            ThemeModeView::Light => Self::Light,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineView {
    pub state: EngineState,
    pub sample_rate: u32,
    /// 全局默认的重采样算法
    pub resampler: ResamplerView,
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
    AppSource,
    ExternalInsert,
    Recorder,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceView {
    pub endpoint_id: String,
    pub name: String,
}

/// 应用源绑定的程序。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgramView {
    pub path: String,
    pub file_name: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeView {
    pub id: u32,
    pub name: String,
    pub kind: NodeKindView,
    pub slot: Option<SlotRef>,
    pub device: Option<DeviceView>,
    /// 应用源绑定的程序
    pub program: Option<ProgramView>,
    /// 应用源按节点覆盖的重采样算法；None 表示跟随全局默认
    pub resampler: Option<ResamplerView>,
    pub x: f64,
    pub y: f64,
    pub gain_db: f32,
    pub muted: bool,
    /// 门限阈值（dBFS）；None 表示关。只有输入节点会有
    pub gate_db: Option<f32>,
    /// 外部设备节点的设备设置
    pub io: Option<DeviceSettingsView>,
    /// 占用槽位的节点的格式与重采样算法
    pub slot_settings: Option<SlotSettingsView>,
    /// 外部设备在线时实际使用的设备格式
    pub format: Option<FormatView>,
    /// 设备换了端点、又有多个同名设备时，可以改用的候选（由用户在节点设置里选）
    pub candidates: Vec<DeviceView>,
    pub status: NodeStatus,
    /// 录音节点的设置与状态
    pub recorder: Option<RecorderView>,
    pub mute_hotkey: Option<HotkeyBindingView>,
    pub record_hotkey: Option<HotkeyBindingView>,
    /// 这个应用源造成了重复路由
    pub duplicate: Option<DuplicateView>,
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
    /// 应用源的程序没有在运行：不算离线（GLOSSARY：未运行）
    NotRunning,
    /// 应用源的程序文件不存在（GLOSSARY：程序缺失），计入离线警告
    ProgramMissing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum OfflineReasonView {
    WaitingForDevice,
    InUseByOtherApp,
    ExclusiveNotAllowed,
    NoSignal,
    /// 槽位正在重新挂载
    SlotRepairing,
    /// 槽位多次挂载失败
    SlotFailing,
}

impl NodeStatus {
    /// 外部设备或应用源节点的状态；引擎还没报告时为等待中。
    pub fn of_endpoint(status: Option<&EndpointStatus>) -> Self {
        match status {
            Some(EndpointStatus::Online { .. }) => Self::Ok,
            Some(EndpointStatus::NotRunning) => Self::NotRunning,
            Some(EndpointStatus::ProgramMissing) => Self::ProgramMissing,
            Some(EndpointStatus::Offline { reason }) => Self::Offline {
                reason: match reason {
                    OfflineReason::WaitingForDevice => OfflineReasonView::WaitingForDevice,
                    OfflineReason::InUseByOtherApp => OfflineReasonView::InUseByOtherApp,
                    OfflineReason::ExclusiveNotAllowed => OfflineReasonView::ExclusiveNotAllowed,
                    OfflineReason::NoSignal => OfflineReasonView::NoSignal,
                },
            },
            None => Self::Pending,
        }
    }

    /// 虚拟节点的状态，取自它所占槽位的挂载状态。
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
    /// 这条连线在重复路径上
    pub duplicate: Option<DuplicateView>,
    pub toggle_hotkey: Option<HotkeyBindingView>,
}

impl RouteView {
    pub fn new(route: &Route, duplicate: Option<DuplicateView>) -> Self {
        Self {
            from: route.from.0,
            to: route.to.0,
            gain_db: route.gain.db(),
            enabled: route.enabled,
            label_position: route.label_position.get(),
            duplicate,
            toggle_hotkey: route.toggle_hotkey.map(HotkeyBindingView::from),
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

/// 设备格式。界面也用它回传用户选定的独占格式（[`crate::device_settings`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FormatView {
    pub sample_rate: u32,
    pub sample_format: SampleFormatView,
    pub channels: u16,
}

impl TryFrom<FormatView> for DeviceFormat {
    type Error = SampleRateError;

    fn try_from(format: FormatView) -> Result<Self, Self::Error> {
        Ok(Self {
            sample_rate: SampleRate::new(format.sample_rate)?,
            sample_format: match format.sample_format {
                SampleFormatView::Int16 => SampleFormat::Int16,
                SampleFormatView::Int24 => SampleFormat::Int24,
                SampleFormatView::Int24In32 => SampleFormat::Int24In32,
                SampleFormatView::Int32 => SampleFormat::Int32,
                SampleFormatView::Float32 => SampleFormat::Float32,
                SampleFormatView::Float64 => SampleFormat::Float64,
            },
            channels: format.channels,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

/// 节点类型在界面上的表示，只保留类型，不带设备或槽位。
pub fn kind_view(kind: &NodeKind) -> NodeKindView {
    match kind {
        NodeKind::ExternalInput { .. } => NodeKindView::ExternalInput,
        NodeKind::VirtualInput { .. } => NodeKindView::VirtualInput,
        NodeKind::VirtualOutput { .. } => NodeKindView::VirtualOutput,
        NodeKind::ExternalOutput { .. } => NodeKindView::ExternalOutput,
        NodeKind::Bus => NodeKindView::Bus,
        NodeKind::AppSource { .. } => NodeKindView::AppSource,
        NodeKind::ExternalInsert { .. } => NodeKindView::ExternalInsert,
        NodeKind::Recorder { .. } => NodeKindView::Recorder,
    }
}

/// 拖线时某个候选节点能否连上；不能时给出原因（界面据此显示提示）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteVerdict {
    pub node: u32,
    pub rejection: Option<RejectionView>,
}

/// 路由矩阵里一个不能新建连线的格子及原因（悬停时显示）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairVerdict {
    pub from: u32,
    pub to: u32,
    pub rejection: RejectionView,
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
