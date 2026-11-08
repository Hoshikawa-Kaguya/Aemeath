//! 界面可以调用的命令（`invoke`）。编辑类命令都返回编辑后的完整状态，界面直接替换。

use aemeath_core::model::Position;
use aemeath_core::preferences::LanguagePreference;
use aemeath_protocol::ids::NodeId;
use serde::Serialize;
use tauri::State;
use tauri::ipc::{Channel, InvokeResponseBody};

use crate::controller::{CommandError, ControllerHandle, DeviceDirection, Edit, NewNodeKind};
use crate::meters::MeterHub;
use crate::tray::TrayMenu;
use crate::view::{RouteVerdict, ViewState};

type Reply<T> = Result<T, CommandError>;

#[tauri::command]
pub async fn get_state(controller: State<'_, ControllerHandle>) -> Reply<ViewState> {
    controller.state().await
}

#[tauri::command]
pub async fn initialize(controller: State<'_, ControllerHandle>, default_input_name: String) -> Reply<ViewState> {
    controller.edit(Edit::Initialize { default_input_name }).await
}

#[tauri::command]
pub async fn add_node(
    controller: State<'_, ControllerHandle>,
    kind: NewNodeKind,
    name: String,
    x: f64,
    y: f64,
) -> Reply<ViewState> {
    controller
        .edit(Edit::AddNode {
            kind,
            name,
            position: Position { x, y },
        })
        .await
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
pub async fn undo(controller: State<'_, ControllerHandle>) -> Reply<ViewState> {
    controller.edit(Edit::Undo).await
}

#[tauri::command]
pub async fn set_language(controller: State<'_, ControllerHandle>, language: LanguagePreference) -> Reply<ViewState> {
    controller.edit(Edit::SetLanguage(language)).await
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
pub fn set_tray_labels(tray: State<'_, TrayMenu>, open: String, quit: String) -> Reply<()> {
    tray.set_labels(&open, &quit).map_err(|error| CommandError::Failed {
        message: error.to_string(),
    })
}
