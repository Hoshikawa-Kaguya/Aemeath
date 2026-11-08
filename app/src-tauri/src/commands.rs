//! 界面可以调用的命令（`invoke`）。编辑类命令都返回编辑后的完整状态，界面直接替换。

use aemeath_core::model::Position;
use aemeath_core::preferences::LanguagePreference;
use aemeath_protocol::ids::{EndpointId, NodeId};
use aemeath_protocol::topology::DeviceSettings;
use serde::Serialize;
use tauri::State;
use tauri::ipc::{Channel, InvokeResponseBody};

use aemeath_sys::policy_config::DefaultRole;

use crate::apps::AppsView;
use crate::controller::{CommandError, ControllerHandle, DeviceDirection, Edit, NewNodeKind};
use crate::default_devices::DefaultDevicesView;
use crate::device_settings::{CapabilitiesView, DeviceSettingsView};
use crate::meters::MeterHub;
use crate::tray::{TrayLabels, TrayMenu};
use crate::view::{PairVerdict, RouteVerdict, ViewState};

type Reply<T> = Result<T, CommandError>;

#[tauri::command]
pub async fn get_state(controller: State<'_, ControllerHandle>) -> Reply<ViewState> {
    controller.state().await
}

/// 使用预设（GLOSSARY：预设）：只在工作台为空时生效。名称由界面按当前语言提供。
#[tauri::command]
pub async fn apply_preset(
    controller: State<'_, ControllerHandle>,
    input_name: String,
    output_name: String,
) -> Reply<ViewState> {
    controller
        .edit(Edit::ApplyPreset {
            input_name,
            output_name,
        })
        .await
}

#[tauri::command]
pub async fn add_node(
    controller: State<'_, ControllerHandle>,
    kind: NewNodeKind,
    name: String,
    x: f64,
    y: f64,
    io: Option<DeviceSettingsView>,
) -> Reply<ViewState> {
    let io = io.map(DeviceSettings::try_from).transpose().map_err(invalid)?;
    controller
        .edit(Edit::AddNode {
            kind,
            name,
            position: Position { x, y },
            io,
        })
        .await
}

/// 外部设备节点的独占、格式、缓冲与重采样算法（ADR-0014）。
#[tauri::command]
pub async fn set_device_settings(
    controller: State<'_, ControllerHandle>,
    node: u32,
    settings: DeviceSettingsView,
) -> Reply<ViewState> {
    let settings = DeviceSettings::try_from(settings).map_err(invalid)?;
    controller
        .edit(Edit::SetDeviceSettings {
            node: NodeId(node),
            settings,
        })
        .await
}

/// 从同名候选里选定外部设备节点要改用的设备（选型报告 6.5 节）。
#[tauri::command]
pub async fn replace_device(
    controller: State<'_, ControllerHandle>,
    node: u32,
    endpoint_id: String,
    device_name: String,
) -> Reply<ViewState> {
    controller
        .edit(Edit::ReplaceDevice {
            node: NodeId(node),
            endpoint_id,
            device_name,
        })
        .await
}

/// 查询外部设备的能力（独占时可选的格式、缓冲）；设备打不开时为 null。
#[tauri::command]
pub async fn probe_device(
    controller: State<'_, ControllerHandle>,
    endpoint_id: String,
    direction: DeviceDirection,
) -> Reply<Option<CapabilitiesView>> {
    controller.probe_device(EndpointId(endpoint_id), direction).await
}

/// 应用页：正在出声的程序与可选的落点。
#[tauri::command]
pub async fn list_apps(controller: State<'_, ControllerHandle>) -> Reply<AppsView> {
    controller.apps().await
}

/// 指派程序的落点；`endpoint_id` 为 null 表示跟随系统默认。`executable` 用于记录这次指派。
#[tauri::command]
pub async fn set_landing(
    controller: State<'_, ControllerHandle>,
    process_id: u32,
    executable: Option<String>,
    endpoint_id: Option<String>,
) -> Reply<()> {
    controller.set_landing(process_id, executable, endpoint_id).await
}

/// 工具页：两个方向的活动端点及 Windows 默认设备。
#[tauri::command]
pub async fn list_default_devices(controller: State<'_, ControllerHandle>) -> Reply<DefaultDevicesView> {
    controller.default_devices().await
}

/// 把端点设为 Windows 的默认设备（`communications` 为 false）或默认通信设备（为 true）。
#[tauri::command]
pub async fn set_default_device(
    controller: State<'_, ControllerHandle>,
    endpoint_id: String,
    communications: bool,
) -> Reply<()> {
    let role = if communications {
        DefaultRole::Communications
    } else {
        DefaultRole::Default
    };
    controller.set_default_device(endpoint_id, role).await
}

/// 立即重新检测离线的外部设备；`node` 为 null 表示全部。
#[tauri::command]
pub fn redetect(controller: State<'_, ControllerHandle>, node: Option<u32>) {
    controller.redetect(node.map(NodeId));
}

fn invalid(error: impl std::fmt::Display) -> CommandError {
    CommandError::Failed {
        message: error.to_string(),
    }
}

#[tauri::command]
pub async fn remove_node(controller: State<'_, ControllerHandle>, node: u32) -> Reply<ViewState> {
    controller.edit(Edit::RemoveNode(NodeId(node))).await
}

#[tauri::command]
pub async fn rename_node(controller: State<'_, ControllerHandle>, node: u32, name: String) -> Reply<ViewState> {
    controller
        .edit(Edit::RenameNode {
            node: NodeId(node),
            name,
        })
        .await
}

#[tauri::command]
pub async fn move_node(controller: State<'_, ControllerHandle>, node: u32, x: f64, y: f64) -> Reply<ViewState> {
    controller
        .edit(Edit::MoveNode {
            node: NodeId(node),
            position: Position { x, y },
        })
        .await
}

#[tauri::command]
pub async fn change_slot(controller: State<'_, ControllerHandle>, node: u32, number: u8) -> Reply<ViewState> {
    controller
        .edit(Edit::ChangeSlot {
            node: NodeId(node),
            number,
        })
        .await
}

/// 拖线开始时调用一次：每个候选节点能否连上、不能时的原因。
/// `from_output` 为 true 表示从输出端口往下游拖，false 表示从输入端口往上游拖。
#[tauri::command]
pub async fn route_verdicts(
    controller: State<'_, ControllerHandle>,
    node: u32,
    from_output: bool,
) -> Reply<Vec<RouteVerdict>> {
    controller.verdicts(NodeId(node), from_output).await
}

/// 路由矩阵中不能新建连线的格子及原因。
#[tauri::command]
pub async fn matrix_verdicts(controller: State<'_, ControllerHandle>) -> Reply<Vec<PairVerdict>> {
    controller.matrix_verdicts().await
}

#[tauri::command]
pub async fn pin_node(controller: State<'_, ControllerHandle>, node: u32, pinned: bool) -> Reply<ViewState> {
    let node = NodeId(node);
    controller
        .edit(if pinned { Edit::Pin(node) } else { Edit::Unpin(node) })
        .await
}

#[tauri::command]
pub async fn move_pin(controller: State<'_, ControllerHandle>, node: u32, index: usize) -> Reply<ViewState> {
    controller
        .edit(Edit::MovePin {
            node: NodeId(node),
            index,
        })
        .await
}

#[tauri::command]
pub async fn connect(controller: State<'_, ControllerHandle>, from: u32, to: u32) -> Reply<ViewState> {
    controller
        .edit(Edit::Connect {
            from: NodeId(from),
            to: NodeId(to),
        })
        .await
}

#[tauri::command]
pub async fn disconnect(controller: State<'_, ControllerHandle>, from: u32, to: u32) -> Reply<ViewState> {
    controller
        .edit(Edit::Disconnect {
            from: NodeId(from),
            to: NodeId(to),
        })
        .await
}

#[tauri::command]
pub async fn set_route_enabled(
    controller: State<'_, ControllerHandle>,
    from: u32,
    to: u32,
    enabled: bool,
) -> Reply<ViewState> {
    controller
        .edit(Edit::SetRouteEnabled {
            from: NodeId(from),
            to: NodeId(to),
            enabled,
        })
        .await
}

#[tauri::command]
pub async fn set_route_gain(
    controller: State<'_, ControllerHandle>,
    from: u32,
    to: u32,
    gain_db: f32,
) -> Reply<ViewState> {
    controller
        .edit(Edit::SetRouteGain {
            from: NodeId(from),
            to: NodeId(to),
            gain_db,
        })
        .await
}

#[tauri::command]
pub async fn set_route_label(
    controller: State<'_, ControllerHandle>,
    from: u32,
    to: u32,
    position: f32,
) -> Reply<ViewState> {
    controller
        .edit(Edit::SetRouteLabel {
            from: NodeId(from),
            to: NodeId(to),
            position,
        })
        .await
}

#[tauri::command]
pub async fn set_node_gain(controller: State<'_, ControllerHandle>, node: u32, gain_db: f32) -> Reply<ViewState> {
    controller
        .edit(Edit::SetNodeGain {
            node: NodeId(node),
            gain_db,
        })
        .await
}

#[tauri::command]
pub async fn set_node_muted(controller: State<'_, ControllerHandle>, node: u32, muted: bool) -> Reply<ViewState> {
    controller
        .edit(Edit::SetNodeMuted {
            node: NodeId(node),
            muted,
        })
        .await
}

/// 设定输入节点的门限阈值（dBFS）；`threshold_db` 为 null 表示关闭门限。
#[tauri::command]
pub async fn set_node_gate(
    controller: State<'_, ControllerHandle>,
    node: u32,
    threshold_db: Option<f32>,
) -> Reply<ViewState> {
    controller
        .edit(Edit::SetNodeGate {
            node: NodeId(node),
            threshold_db,
        })
        .await
}

#[tauri::command]
pub async fn undo(controller: State<'_, ControllerHandle>) -> Reply<ViewState> {
    controller.edit(Edit::Undo).await
}

#[tauri::command]
pub async fn set_language(controller: State<'_, ControllerHandle>, language: LanguagePreference) -> Reply<ViewState> {
    controller.edit(Edit::SetLanguage(language)).await
}

#[tauri::command]
pub async fn set_last_page(controller: State<'_, ControllerHandle>, page: String) -> Reply<ViewState> {
    controller.edit(Edit::SetLastPage(page)).await
}

/// 一台可选的外部设备。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceChoice {
    pub endpoint_id: String,
    pub name: String,
}

#[tauri::command]
pub async fn list_devices(
    controller: State<'_, ControllerHandle>,
    direction: DeviceDirection,
) -> Reply<Vec<DeviceChoice>> {
    Ok(controller
        .devices(direction)
        .await?
        .into_iter()
        .map(|device| DeviceChoice {
            endpoint_id: device.endpoint_id,
            name: device.name,
        })
        .collect())
}

/// 立即检查一轮槽位挂载（设备页的"立即检查"）。
#[tauri::command]
pub fn check_slots(controller: State<'_, ControllerHandle>) {
    controller.check_slots();
}

#[tauri::command]
pub fn watch_meters(meters: State<'_, MeterHub>, channel: Channel<InvokeResponseBody>) {
    meters.subscribe(channel);
}

#[tauri::command]
pub fn set_tray_labels(tray: State<'_, TrayMenu>, labels: TrayLabels) -> Reply<()> {
    tray.set_labels(&labels).map_err(invalid)
}
