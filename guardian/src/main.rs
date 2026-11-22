//! 守护进程：第一个启动的进程。
//!
//! 1. 每个用户只运行一个。已有实例时请已运行的主程序显示窗口，然后退出。
//! 2. 检查 usbip-win2 驱动；不可用时显示说明对话框并退出，不拉起引擎和主程序。
//! 3. 拉起并监视引擎（`aemeath-host.exe`）与主程序（`aemeath-app.exe`），两者与守护进程放在同一目录，
//!    并收到与守护进程相同的参数（数据目录、管道名、共享内存名；显式指定端口时一并透传）。
//!
//! 用法与引擎相同：`Aemeath [--data-dir <目录>] [--usbip-port <端口>] [--pipe <管道名>] [--meters <共享内存名>]`。

// 发布版不弹出控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// 用户看到的程序名是 Aemeath.exe，二进制名就是 crate 名
#![allow(non_snake_case)]

mod bootstrap;
mod cleanup;
mod driver;
mod elevated;
mod messages;
mod supervisor;

use std::ffi::OsString;
use std::io;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode};

use aemeath_control::usbip_cli::UsbipCli;
use aemeath_core::store::ConfigStore;
use aemeath_runtime::autostart;
use aemeath_runtime::config::{PIPE_ARGUMENT, RuntimeConfig};
use aemeath_runtime::logging::{self, Process};
use aemeath_runtime::names::{APP_EXE, ENGINE_EXE, ProcessNames};
use aemeath_sys::activation;
use aemeath_sys::process::CREATE_NO_WINDOW;
use aemeath_sys::single_instance::SingleInstance;
use aemeath_sys::ui_language;
use tracing::{error, info, warn};
use velopack::VelopackApp;

use crate::supervisor::{Role, Timing};

/// 驱动不可用时的退出码（便于安装程序和脚本区分）
const EXIT_DRIVER_UNAVAILABLE: u8 = 2;

fn main() -> ExitCode {
    // 必须最先运行：Velopack 用自己的参数调用主程序（安装、卸载等钩子），放在后面会被当作未知参数拒绝
    VelopackApp::build()
        .set_auto_apply_on_startup(false)
        .on_before_uninstall_fast_callback(|_version| cleanup::launch_from_temp())
        .run();
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = elevated::dispatch(&raw_args) {
        return code;
    }
    let config = match RuntimeConfig::from_args(raw_args) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("aemeath-guardian: {error}");
            return ExitCode::FAILURE;
        }
    };
    let _logs = match logging::init(&config.data_dir, Process::Guardian) {
        Ok(guard) => guard,
        Err(error) => {
            eprintln!("aemeath-guardian: cannot open the log directory: {error}");
            return ExitCode::FAILURE;
        }
    };
    info!(version = aemeath_runtime::VERSION, data_dir = %config.data_dir.display(), "guardian starting");

    let _instance = match single_instance(&config) {
        Ok(Some(instance)) => instance,
        Ok(None) => {
            info!("another guardian is already running, exiting");
            // 开机自启动重复时只退出，不弹窗口
            if !config.autostart {
                activate_app(&config);
            }
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            error!(%error, "cannot create the single-instance mutex");
            return ExitCode::FAILURE;
        }
    };

    // 守护进程最先启动：配置要升级时在这里写回，之后各处读到的都是当前版本
    match ConfigStore::new(&config.data_dir).upgrade() {
        Ok(true) => info!("upgraded the configuration file"),
        Ok(false) => {}
        Err(error) => warn!(%error, "cannot upgrade the configuration file"),
    }

    if let Err(problem) = check_driver(&config) {
        error!(?problem, "usbip-win2 is not usable");
        let preferences = ConfigStore::new(&config.data_dir).peek_preferences();
        let language = messages::pick_language(
            preferences.as_ref().map(|preferences| &preferences.language),
            &ui_language::preferred_ui_languages(),
        );
        if !bootstrap::resolve(&config, &problem, language, || check_driver(&config)) {
            return ExitCode::from(EXIT_DRIVER_UNAVAILABLE);
        }
    }

    let directory = match std::env::current_exe() {
        Ok(path) => path.parent().map(Path::to_path_buf).unwrap_or_default(),
        Err(error) => {
            error!(%error, "cannot locate the guardian executable");
            return ExitCode::FAILURE;
        }
    };
    let arguments = forwarded_arguments(&config);
    let report = supervisor::supervise(|role| launch(&directory, role, &arguments), &Timing::default());
    info!(?report, "guardian stopped");
    ExitCode::SUCCESS
}

/// 取得本用户的单实例锁；已有实例在运行时返回 None。
fn single_instance(config: &RuntimeConfig) -> io::Result<Option<SingleInstance>> {
    SingleInstance::acquire(&config.identity.guardian_instance_name()?)
}

/// 请已在运行的主程序显示窗口（用户从开始菜单等入口重复启动时）。
/// 主程序正在被重新拉起、还没开始等待时，这次启动没有效果。
fn activate_app(config: &RuntimeConfig) {
    let requested = config
        .identity
        .app_activation_name()
        .and_then(|name| activation::request_activation(&name));
    match requested {
        Ok(true) => info!("asked the running app to show its window"),
        Ok(false) => info!("no app is running to show"),
        Err(error) => error!(%error, "cannot ask the running app to show its window"),
    }
}

/// 收集驱动的各项观测并判定能否启动。
fn check_driver(config: &RuntimeConfig) -> Result<(), driver::DriverProblem> {
    let cli = UsbipCli::default_program()
        .filter(|program| program.exists())
        .map(|program| UsbipCli::new(program, config.usbip_port));
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            return Err(driver::DriverProblem::Incompatible {
                detail: format!("cannot start the async runtime: {error}"),
            });
        }
    };
    let version = runtime.block_on(driver::check(cli.as_ref()))?;
    info!(%version, "usbip-win2 is usable");
    Ok(())
}

/// 引擎和主程序收到与守护进程相同的配置。
/// 仅在端口被明确指定时转发 `--usbip-port`；未指定时由引擎按身份默认端口绑定，不可用时自动改由系统分配，并在握手时通知主程序。
fn forwarded_arguments(config: &RuntimeConfig) -> Vec<OsString> {
    let mut arguments: Vec<OsString> = vec![
        "--data-dir".into(),
        config.data_dir.clone().into_os_string(),
        PIPE_ARGUMENT.into(),
        config.pipe_name.clone().into(),
        "--meters".into(),
        config.meter_memory_name.clone().into(),
    ];
    if config.usbip_port_explicit {
        arguments.push("--usbip-port".into());
        arguments.push(config.usbip_port.to_string().into());
    }
    if config.autostart {
        arguments.push(autostart::ARGUMENT.into());
    }
    arguments
}

/// 启动同目录下的引擎或主程序，不弹控制台窗口。
fn launch(directory: &Path, role: Role, arguments: &[OsString]) -> io::Result<Child> {
    let program: PathBuf = directory.join(match role {
        Role::Engine => ENGINE_EXE,
        Role::App => APP_EXE,
    });
    Command::new(program)
        .args(arguments)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aemeath_usbip::identity::IdentityProfile;

    fn sample_config(usbip_port_explicit: bool) -> RuntimeConfig {
        RuntimeConfig {
            data_dir: PathBuf::from(r"C:\data"),
            usbip_port: 34567,
            usbip_port_explicit,
            identity: IdentityProfile::Development,
            pipe_name: r"\\.\pipe\test".to_owned(),
            meter_memory_name: r"Local\meters".to_owned(),
            autostart: false,
        }
    }

    #[test]
    fn forwards_usbip_port_only_when_explicitly_configured() {
        let implicit = forwarded_arguments(&sample_config(false));
        assert!(!implicit.iter().any(|arg| arg == "--usbip-port"));

        let explicit = forwarded_arguments(&sample_config(true));
        let flag = explicit.iter().position(|arg| arg == "--usbip-port");
        assert_eq!(
            flag.and_then(|index| explicit.get(index + 1)),
            Some(&OsString::from("34567"))
        );
    }
}
