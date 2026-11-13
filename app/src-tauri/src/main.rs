//! 主程序：Rust 后端负责控制面，前端运行在 WebView2 里，窗口完全由前端绘制。
//!
//! 用法与引擎相同：`aemeath-app [--data-dir <目录>] [--usbip-port <端口>] [--pipe <管道名>] [--meters <共享内存名>]`，
//! 正式运行时由守护进程拉起并传入与引擎相同的参数；被直接启动或重复启动时的处理见 `instance`。
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
mod instance;
mod meters;
mod program_icon;
mod recording;
mod tray;
mod updates;
mod view;
mod window;

use std::process::ExitCode;

use aemeath_core::store::ConfigStore;
use aemeath_runtime::config::RuntimeConfig;
use aemeath_runtime::logging::{self, Process};
use tauri::{Emitter, Manager, RunEvent, WindowEvent};

use crate::controller::ControllerHandle;
use crate::instance::Startup;
use crate::meters::MeterHub;
use crate::updates::UpdateService;

/// 状态快照的事件名（界面监听）
const STATE_EVENT: &str = "state-changed";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let config = match RuntimeConfig::from_args(args.clone()) {
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
    tracing::info!(version = aemeath_runtime::VERSION, data_dir = %config.data_dir.display(), "app starting");
    let activation = match instance::claim(&config, &args) {
        Startup::Run(activation) => activation,
        Startup::Exit => return ExitCode::SUCCESS,
    };

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .register_asynchronous_uri_scheme_protocol(program_icon::SCHEME, |_context, request, responder| {
            let path = request.uri().path().to_owned();
            tauri::async_runtime::spawn_blocking(move || responder.respond(program_icon::respond(&path)));
        })
        .setup(move |app| {
            if let Some(event) = activation {
                let handle = app.handle().clone();
                std::thread::spawn(move || instance::show_on_activation(&event, &handle));
            }
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
            app.manage(window::DataDir(config.data_dir.clone()));
            let background = config.data_dir.join(controller::BACKGROUND_FOLDER);
            let allowed = std::fs::create_dir_all(&background)
                .map_err(|error| error.to_string())
                .and_then(|()| {
                    app.asset_protocol_scope()
                        .allow_directory(&background, false)
                        .map_err(|error| error.to_string())
                });
            if let Err(error) = allowed {
                tracing::warn!(%error, "cannot allow the background image folder");
            }
            app.manage(UpdateService::start(app.handle().clone()));
            // 开机自启动时只留在托盘，窗口等用户点托盘图标再创建
            if !config.autostart {
                tray::show_main_window(app.handle());
            }
            let startup = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let wanted = startup.state::<ControllerHandle>().state().await;
                if wanted.is_ok_and(|state| state.check_updates_on_start) {
                    startup.state::<UpdateService>().check();
                }
            });
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
            commands::set_global_resampler,
            commands::set_engine_rate,
            commands::set_clock_master,
            commands::open_app_volume_settings,
            commands::get_update_state,
            commands::check_updates,
            commands::download_update,
            commands::apply_update,
            commands::open_releases_page,
            commands::set_check_updates_on_start,
            commands::autostart_enabled,
            commands::set_autostart,
            commands::crash_dumps_enabled,
            commands::enable_crash_dumps,
            commands::remove_virtual_devices,
            commands::window_ready,
            commands::set_appearance,
            commands::set_background_image,
            commands::remove_background_image,
            commands::open_data_folder,
            commands::open_log_folder,
            commands::set_slot_settings,
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
            commands::set_hotkeys_paused,
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
            commands::set_route_label_visible,
            commands::set_node_gain,
            commands::set_node_muted,
            commands::set_node_gate,
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
