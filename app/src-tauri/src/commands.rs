//! 界面可以调用的命令（`invoke`）。编辑类命令都返回编辑后的完整状态，界面直接替换。

use aemeath_control::crash_dumps;
use aemeath_control::slots::uninstall;
use aemeath_core::hotkeys::HotkeyBinding;
use aemeath_core::model::Position;
use aemeath_core::preferences::{LanguagePreference, ThemeMode};
use aemeath_protocol::ids::{EndpointId, NodeId};
use aemeath_protocol::topology::{DeviceSettings, Resampler, SlotNodeSettings};
use aemeath_protocol::units::SampleRate;
use aemeath_runtime::autostart;
use aemeath_runtime::logging::LOG_FOLDER;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, State, WebviewWindow};

use aemeath_sys::elevation::{ElevateError, run_elevated};
use aemeath_sys::policy_config::DefaultRole;

use crate::apps::AppsView;
use crate::controller::{BACKGROUND_FOLDER, CommandError, ControllerHandle, DeviceDirection, Edit, NewNodeKind};
use crate::default_devices::DefaultDevicesView;
use crate::device_settings::{CapabilitiesView, DeviceSettingsView, ResamplerView, SlotSettingsView};
use crate::hotkey_views::{HotkeyBindingView, HotkeyTargetView};
use crate::meters::MeterHub;
use crate::recording::RecorderSettingsView;
use crate::tray::{TrayLabels, TrayMenu};
use crate::updates::{UpdateService, UpdateView};
use crate::view::{PairVerdict, RouteVerdict, ThemeModeView, ViewState};
use crate::window::DataDir;

type Reply<T> = Result<T, CommandError>;

/// 守护进程的文件名（M4 计划 P1）
const GUARDIAN_EXE: &str = "Aemeath.exe";
/// `#rrggbb` 的长度
const HEX_COLOR_LENGTH: usize = 7;
const IMAGE_EXTENSIONS: [&str; 4] = ["png", "jpg", "jpeg", "webp"];
/// 背景图的大小上限（M4 计划 V8）
const MAX_BACKGROUND_BYTES: u64 = 40 * 1024 * 1024;

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
    slot_settings: Option<SlotSettingsView>,
) -> Reply<ViewState> {
    let io = io.map(DeviceSettings::try_from).transpose().map_err(invalid)?;
    let slot_settings = slot_settings
        .map(SlotNodeSettings::try_from)
        .transpose()
        .map_err(invalid)?;
    controller
        .edit(Edit::AddNode {
            kind,
            name,
            position: Position { x, y },
            io,
            slot_settings,
        })
        .await
}

/// 占用槽位的节点的格式与重采样算法（M4 计划 F1）。
#[tauri::command]
pub async fn set_slot_settings(
    controller: State<'_, ControllerHandle>,
    node: u32,
    settings: SlotSettingsView,
) -> Reply<ViewState> {
    let settings = SlotNodeSettings::try_from(settings).map_err(invalid)?;
    controller
        .edit(Edit::SetSlotSettings {
            node: NodeId(node),
            settings,
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

/// 应用源的默认节点名（文件说明，或不带扩展名的文件名）。
#[tauri::command]
pub fn describe_program(path: String) -> String {
    crate::apps::default_program_name(&path)
}

/// 让应用源改绑另一个程序。
#[tauri::command]
pub async fn replace_program(controller: State<'_, ControllerHandle>, node: u32, program: String) -> Reply<ViewState> {
    controller
        .edit(Edit::ReplaceProgram {
            node: NodeId(node),
            program,
        })
        .await
}

/// 应用源按节点覆盖的重采样算法；null 表示跟随全局默认。
#[tauri::command]
pub async fn set_app_resampler(
    controller: State<'_, ControllerHandle>,
    node: u32,
    resampler: Option<ResamplerView>,
) -> Reply<ViewState> {
    controller
        .edit(Edit::SetAppResampler {
            node: NodeId(node),
            resampler: resampler.map(Resampler::from),
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

/// 开始录音（M3 计划 R4）。
#[tauri::command]
pub async fn start_recording(controller: State<'_, ControllerHandle>, node: u32) -> Reply<()> {
    controller.start_recording(NodeId(node)).await
}

#[tauri::command]
pub async fn stop_recording(controller: State<'_, ControllerHandle>, node: u32) -> Reply<()> {
    controller.stop_recording(NodeId(node)).await
}

#[tauri::command]
pub async fn set_recorder_settings(
    controller: State<'_, ControllerHandle>,
    node: u32,
    settings: RecorderSettingsView,
) -> Reply<ViewState> {
    controller
        .edit(Edit::SetRecorderSettings {
            node: NodeId(node),
            settings: settings.into(),
        })
        .await
}

#[tauri::command]
pub async fn set_recording_folder(controller: State<'_, ControllerHandle>, folder: String) -> Reply<ViewState> {
    controller.edit(Edit::SetRecordingFolder(folder)).await
}

#[tauri::command]
pub async fn set_recording_defaults(
    controller: State<'_, ControllerHandle>,
    settings: RecorderSettingsView,
) -> Reply<ViewState> {
    controller.edit(Edit::SetRecordingDefaults(settings.into())).await
}

/// 在资源管理器里打开录音文件夹（不存在时先创建）。
#[tauri::command]
pub async fn open_recording_folder(controller: State<'_, ControllerHandle>) -> Reply<()> {
    let state = controller.state().await?;
    open_in_explorer(Path::new(&state.recording.folder))
}

#[tauri::command]
pub fn get_update_state(updates: State<'_, UpdateService>) -> UpdateView {
    updates.view()
}

#[tauri::command]
pub fn check_updates(updates: State<'_, UpdateService>) {
    updates.check();
}

#[tauri::command]
pub fn download_update(updates: State<'_, UpdateService>) {
    updates.download();
}

#[tauri::command]
pub async fn apply_update(updates: State<'_, UpdateService>, controller: State<'_, ControllerHandle>) -> Reply<()> {
    updates.apply(&controller).await
}

#[tauri::command]
pub fn open_releases_page(updates: State<'_, UpdateService>) -> Reply<()> {
    std::process::Command::new("explorer")
        .arg(updates.releases_url())
        .spawn()
        .map_err(invalid)?;
    Ok(())
}

#[tauri::command]
pub async fn set_check_updates_on_start(controller: State<'_, ControllerHandle>, enabled: bool) -> Reply<ViewState> {
    controller.edit(Edit::SetCheckUpdatesOnStart(enabled)).await
}

/// 守护进程的路径：与主程序在同一目录。
fn guardian_program() -> Reply<PathBuf> {
    Ok(std::env::current_exe().map_err(invalid)?.with_file_name(GUARDIAN_EXE))
}

#[tauri::command]
pub fn autostart_enabled() -> bool {
    guardian_program().is_ok_and(|program| autostart::is_enabled(&program).unwrap_or(false))
}

/// 打开或关闭开机自启动，返回设置后的状态。
#[tauri::command]
pub fn set_autostart(enabled: bool) -> Reply<bool> {
    if !enabled {
        autostart::disable().map_err(invalid)?;
        return Ok(false);
    }
    let program = guardian_program()?;
    // 不写指向不存在文件的启动项（例如开发时只编译了主程序）
    std::fs::metadata(&program).map_err(invalid)?;
    autostart::enable(&program).map_err(invalid)?;
    Ok(true)
}

/// 崩溃转储是否已启用（M4 计划 W3）。
#[tauri::command]
pub fn crash_dumps_enabled() -> bool {
    crash_dumps::is_enabled().unwrap_or(false)
}

/// 经 UAC 启用崩溃转储。返回启用后的状态；用户在 UAC 里选"否"时返回当前状态。
#[tauri::command]
pub async fn enable_crash_dumps() -> Reply<bool> {
    let program = guardian_program()?;
    tauri::async_runtime::spawn_blocking(move || match run_elevated(&program, crash_dumps::ENABLE_ARGUMENT) {
        Ok(process) => {
            process.wait().map_err(invalid)?;
            Ok(crash_dumps::is_enabled().unwrap_or(false))
        }
        Err(ElevateError::Cancelled) => Ok(crash_dumps::is_enabled().unwrap_or(false)),
        Err(error) => Err(invalid(error)),
    })
    .await
    .map_err(invalid)?
}

/// 便携版"移除虚拟设备"（M4 计划 C4）：启动提权的清理进程后整体退出。用户在 UAC 里选"否"时什么都不做。
#[tauri::command]
pub async fn remove_virtual_devices(app: AppHandle) -> Reply<()> {
    let program = guardian_program()?;
    let started =
        tauri::async_runtime::spawn_blocking(move || run_elevated(&program, uninstall::REMOVE_ARGUMENT).map(drop))
            .await
            .map_err(invalid)?;
    match started {
        Ok(()) => {
            crate::tray::quit_app(&app);
            Ok(())
        }
        Err(ElevateError::Cancelled) => Ok(()),
        Err(error) => Err(invalid(error)),
    }
}

/// 界面应用好主题后调用，这时才显示窗口。
#[tauri::command]
pub fn window_ready(window: WebviewWindow) {
    crate::window::show(&window);
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppearanceInput {
    theme_mode: ThemeModeView,
    accent: Option<String>,
    overlay_percent: u8,
}

fn is_hex_color(text: &str) -> bool {
    text.len() == HEX_COLOR_LENGTH && text.starts_with('#') && text[1..].chars().all(|c| c.is_ascii_hexdigit())
}

#[tauri::command]
pub async fn set_appearance(controller: State<'_, ControllerHandle>, appearance: AppearanceInput) -> Reply<ViewState> {
    if appearance.accent.as_deref().is_some_and(|accent| !is_hex_color(accent)) {
        return Err(invalid("the accent color must be #rrggbb"));
    }
    controller
        .edit(Edit::SetAppearance {
            theme_mode: ThemeMode::from(appearance.theme_mode),
            accent: appearance.accent,
            overlay_percent: appearance.overlay_percent,
        })
        .await
}

fn is_supported_image(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            IMAGE_EXTENSIONS
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

/// 校验后把图片复制进数据目录（M4 计划 V8），原文件之后移动或删除都不受影响。文件名带时间，换图时浏览器不会用旧缓存。
fn copy_background(source: &Path, folder: &Path) -> Result<String, String> {
    if !is_supported_image(source) {
        return Err("only PNG, JPG and WebP images are supported".to_owned());
    }
    let size = std::fs::metadata(source).map_err(|error| error.to_string())?.len();
    if size > MAX_BACKGROUND_BYTES {
        return Err("the image is larger than 40 MB".to_owned());
    }
    std::fs::create_dir_all(folder).map_err(|error| error.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    let extension = source
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default();
    let name = format!("image-{stamp}.{}", extension.to_ascii_lowercase());
    std::fs::copy(source, folder.join(&name)).map_err(|error| error.to_string())?;
    remove_other_files(folder, Some(&name));
    Ok(name)
}

fn remove_other_files(folder: &Path, keep: Option<&str>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        if keep != entry.file_name().to_str() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[tauri::command]
pub async fn set_background_image(
    controller: State<'_, ControllerHandle>,
    data_dir: State<'_, DataDir>,
    path: String,
) -> Reply<ViewState> {
    let folder = data_dir.0.join(BACKGROUND_FOLDER);
    let name = tauri::async_runtime::spawn_blocking(move || copy_background(Path::new(&path), &folder))
        .await
        .map_err(invalid)?
        .map_err(invalid)?;
    controller.edit(Edit::SetBackgroundFile(Some(name))).await
}

#[tauri::command]
pub async fn remove_background_image(
    controller: State<'_, ControllerHandle>,
    data_dir: State<'_, DataDir>,
) -> Reply<ViewState> {
    let state = controller.edit(Edit::SetBackgroundFile(None)).await?;
    remove_other_files(&data_dir.0.join(BACKGROUND_FOLDER), None);
    Ok(state)
}

/// 设置页"打开配置目录"（M4 计划 D4）。
#[tauri::command]
pub fn open_data_folder(data_dir: State<'_, DataDir>) -> Reply<()> {
    open_in_explorer(&data_dir.0)
}

/// 设置页"打开日志目录"（M4 计划 D4）。
#[tauri::command]
pub fn open_log_folder(data_dir: State<'_, DataDir>) -> Reply<()> {
    open_in_explorer(&data_dir.0.join(LOG_FOLDER))
}

fn open_in_explorer(folder: &Path) -> Reply<()> {
    std::fs::create_dir_all(folder).map_err(invalid)?;
    std::process::Command::new("explorer")
        .arg(folder)
        .spawn()
        .map_err(invalid)?;
    Ok(())
}

/// 设定或清除一个热键；`binding` 为 null 表示清除。
#[tauri::command]
pub async fn set_hotkey(
    controller: State<'_, ControllerHandle>,
    target: HotkeyTargetView,
    binding: Option<HotkeyBindingView>,
) -> Reply<ViewState> {
    controller
        .edit(Edit::SetHotkey {
            target: target.into(),
            binding: binding.map(HotkeyBinding::from),
        })
        .await
}

/// 界面录入新按键期间暂停全部热键（M3 计划 H12）。
#[tauri::command]
pub fn set_hotkeys_paused(controller: State<'_, ControllerHandle>, paused: bool) {
    controller.set_hotkeys_paused(paused);
}

/// 打开或解除紧急静音。
#[tauri::command]
pub async fn set_panic_mute(controller: State<'_, ControllerHandle>, active: bool) -> Reply<ViewState> {
    controller.edit(Edit::SetPanicMute(active)).await
}

/// 落在这个节点上的应用数（删除节点前的提示用）。
#[tauri::command]
pub async fn landing_count(controller: State<'_, ControllerHandle>, node: u32) -> Reply<usize> {
    controller.landing_count(NodeId(node)).await
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
pub async fn set_engine_rate(controller: State<'_, ControllerHandle>, hz: u32) -> Reply<ViewState> {
    let rate = SampleRate::new(hz).map_err(invalid)?;
    controller.edit(Edit::SetEngineRate(rate)).await
}

#[tauri::command]
pub async fn set_global_resampler(
    controller: State<'_, ControllerHandle>,
    resampler: ResamplerView,
) -> Reply<ViewState> {
    controller
        .edit(Edit::SetGlobalResampler(Resampler::from(resampler)))
        .await
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
