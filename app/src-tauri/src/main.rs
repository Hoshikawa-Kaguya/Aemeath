//! 主程序（ADR-0003、ADR-0013）：Rust 后端负责控制面，前端运行在 WebView2 里，窗口完全由前端绘制。
//!
//! 用法与引擎相同：`aemeath [--data-dir <目录>] [--usbip-port <端口>] [--pipe <管道名>] [--meters <共享内存名>]`，
//! 正式运行时由守护进程拉起并传入与引擎相同的参数。
//!
//! 关闭窗口时销毁 WebView，程序留在托盘；从托盘退出时请引擎停止，再正常退出（退出码 0）。

// 发布版不弹出控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod apps;
mod commands;
mod controller;
mod default_devices;
mod device_settings;
mod hotkey_views;
mod meters;
mod recording;
mod tray;
mod view;
mod window;

use std::process::ExitCode;

use aemeath_core::store::ConfigStore;
use aemeath_runtime::config::RuntimeConfig;
use aemeath_runtime::logging::{self, Process};
use tauri::{Emitter, Manager, RunEvent, WindowEvent};

use crate::controller::ControllerHandle;
use crate::meters::MeterHub;

/// 状态快照的事件名（界面监听）
const STATE_EVENT: &str = "state-changed";

fn main() -> ExitCode {
    let config = match RuntimeConfig::from_args(std::env::args().skip(1)) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("aemeath: {error}");
            return ExitCode::FAILURE;
        }
    };
    let _logs = match logging::init(&config.data_dir, Process::App) {
        Ok(guard) => guard,
        Err(error) => {
            eprintln!("aemeath: cannot open the log directory: {error}");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(version = env!("CARGO_PKG_VERSION"), data_dir = %config.data_dir.display(), "app starting");

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            let handle = app.handle().clone();
            let toggle_handle = app.handle().clone();
            let toggle_window = move || {
                let app = toggle_handle.clone();
                // 窗口操作要在主线程上执行
                let target = app.clone();
                let _ = app.run_on_main_thread(move || tray::toggle_main_window(&target));
            };
            let controller = ControllerHandle::start(config.clone(), Box::new(toggle_window), move |state| {
                if let Err(error) = handle.emit(STATE_EVENT, state) {
                    tracing::debug!(%error, "cannot push the state to the window");
                }
            });
            app.manage(controller);
            app.manage(MeterHub::start(config.meter_memory_name.clone()));
            app.manage(tray::create(app.handle())?);
            app.manage(ConfigStore::new(&config.data_dir));
            if let Some(window) = app.get_webview_window(tray::MAIN_WINDOW) {
                window::prepare(&window);
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if matches!(event, WindowEvent::CloseRequested { .. }) {
                let app = window.app_handle().clone();
                let placement = window::capture(&app);
                tauri::async_runtime::spawn(window::remember(app, placement));
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_state,
            commands::apply_preset,
            commands::add_node,
            commands::remove_node,
            commands::rename_node,
            commands::move_node,
            commands::change_slot,
            commands::set_device_settings,
            commands::replace_device,
            commands::describe_program,
            commands::replace_program,
            commands::set_app_resampler,
            commands::probe_device,
            commands::redetect,
            commands::list_apps,
            commands::set_landing,
            commands::landing_count,
            commands::set_hotkey,
            commands::capture_hotkey,
            commands::set_panic_mute,
            commands::start_recording,
            commands::stop_recording,
            commands::set_recorder_settings,
            commands::set_recording_folder,
            commands::set_recording_defaults,
            commands::open_recording_folder,
            commands::list_default_devices,
            commands::set_default_device,
            commands::route_verdicts,
            commands::matrix_verdicts,
            commands::pin_node,
            commands::move_pin,
            commands::connect,
            commands::disconnect,
            commands::set_route_enabled,
            commands::set_route_gain,
            commands::set_route_label,
            commands::set_node_gain,
            commands::set_node_muted,
            commands::set_node_gate,
            commands::undo,
            commands::set_language,
            commands::set_last_page,
            commands::list_devices,
            commands::check_slots,
            commands::watch_meters,
            commands::set_tray_labels,
        ])
        .build(tauri::generate_context!());
    let app = match app {
        Ok(app) => app,
        Err(error) => {
            tracing::error!(%error, "Aemeath failed to start");
            return ExitCode::FAILURE;
        }
    };
    app.run(|_app, event| {
        // 关闭最后一个窗口时不退出，留在托盘；只有从托盘退出（app.exit）才真正退出
        if let RunEvent::ExitRequested { code: None, api, .. } = event {
            api.prevent_exit();
        }
    });
    tracing::info!("app stopped");
    ExitCode::SUCCESS
}
