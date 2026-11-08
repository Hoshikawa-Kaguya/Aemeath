//! 托盘（ADR-0013）：关闭窗口时销毁 WebView，只留托盘和后端，降低常驻内存；从托盘重新打开时重建窗口。
//!
//! 菜单文字由界面按当前语言提供（`set_tray_labels`），界面加载之前用英文。

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, WebviewWindowBuilder, Wry};
use tracing::{error, info};

use crate::controller::ControllerHandle;

pub const MAIN_WINDOW: &str = "main";
const MENU_OPEN: &str = "open";
const MENU_REDETECT: &str = "redetect";
const MENU_QUIT: &str = "quit";
const DEFAULT_OPEN_LABEL: &str = "Open Aemeath";
const DEFAULT_REDETECT_LABEL: &str = "Re-detect all devices";
const DEFAULT_QUIT_LABEL: &str = "Quit Aemeath";
const TOOLTIP: &str = "Aemeath";

/// 托盘菜单项，供界面换语言时更新文字（Tauri 托管状态）。
pub struct TrayMenu {
    open: MenuItem<Wry>,
    redetect: MenuItem<Wry>,
    quit: MenuItem<Wry>,
}

/// 托盘菜单的文字（界面按当前语言提供）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct TrayLabels {
    pub open: String,
    pub redetect: String,
    pub quit: String,
}

impl TrayMenu {
    pub fn set_labels(&self, labels: &TrayLabels) -> tauri::Result<()> {
        self.open.set_text(&labels.open)?;
        self.redetect.set_text(&labels.redetect)?;
        self.quit.set_text(&labels.quit)
    }
}

pub fn create(app: &AppHandle) -> tauri::Result<TrayMenu> {
    let open = MenuItem::with_id(app, MENU_OPEN, DEFAULT_OPEN_LABEL, true, None::<&str>)?;
    // 选型报告 6.5 节：托盘菜单里有"重新检测全部设备"
    let redetect = MenuItem::with_id(app, MENU_REDETECT, DEFAULT_REDETECT_LABEL, true, None::<&str>)?;
    let quit = MenuItem::with_id(app, MENU_QUIT, DEFAULT_QUIT_LABEL, true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&open, &redetect, &separator, &quit])?;
    let mut builder = TrayIconBuilder::with_id(MAIN_WINDOW)
        .tooltip(TOOLTIP)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            MENU_OPEN => show_main_window(app),
            MENU_REDETECT => {
                if let Some(controller) = app.try_state::<ControllerHandle>() {
                    controller.redetect(None);
                }
            }
            MENU_QUIT => quit_app(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(TrayMenu { open, redetect, quit })
}

/// 显示主窗口；窗口已被关闭（销毁）时按配置重建。
pub fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    let Some(config) = app
        .config()
        .app
        .windows
        .iter()
        .find(|window| window.label == MAIN_WINDOW)
    else {
        error!("the main window is missing from tauri.conf.json");
        return;
    };
    match WebviewWindowBuilder::from_config(app, config).and_then(|builder| builder.build()) {
        Ok(window) => {
            crate::window::prepare(&window);
            let _ = window.set_focus();
        }
        Err(error) => error!(%error, "cannot reopen the main window"),
    }
}

/// 显示/隐藏主窗口（全局热键）：开着就关（与点关闭按钮相同，销毁窗口留托盘），没开就打开。
pub fn toggle_main_window(app: &AppHandle) {
    match app.get_webview_window(MAIN_WINDOW) {
        Some(window) => {
            let _ = window.close();
        }
        None => show_main_window(app),
    }
}

/// 用户从托盘退出：请引擎停止，然后退出主程序（守护进程见到正常退出就整体停止）。
fn quit_app(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        crate::window::remember(app.clone(), crate::window::capture(&app)).await;
        let confirmed = match app.try_state::<ControllerHandle>() {
            Some(controller) => controller.shutdown_engine().await,
            None => false,
        };
        info!(engine_confirmed = confirmed, "quitting from the tray");
        app.exit(0);
    });
}
