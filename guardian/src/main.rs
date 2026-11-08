//! 守护进程（ADR-0003、ADR-0011）：第一个启动的进程。
//!
//! 1. 每个用户只运行一个（已有实例时直接退出）。
//! 2. 检查 usbip-win2 驱动；不可用时显示说明对话框并退出，不拉起引擎和主程序。
//! 3. 拉起并监视引擎（`aemeath-host.exe`）与主程序（`aemeath.exe`），两者与守护进程放在同一目录，
//!    并收到与守护进程相同的参数（数据目录、端口、管道名、共享内存名）。
//!
//! 用法与引擎相同：`aemeath-guardian [--data-dir <目录>] [--usbip-port <端口>] [--pipe <管道名>] [--meters <共享内存名>]`。

// 发布版不弹出控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod driver;
mod messages;
mod supervisor;

use std::ffi::OsString;
use std::io;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode};

use aemeath_control::usbip_cli::UsbipCli;
use aemeath_core::store::ConfigStore;
use aemeath_runtime::config::RuntimeConfig;
use aemeath_runtime::logging::{self, Process};
use aemeath_sys::single_instance::SingleInstance;
use aemeath_sys::{task_dialog, ui_language};
use tracing::{error, info};

use crate::supervisor::{Role, Timing};

const ENGINE_EXE: &str = "aemeath-host.exe";
const APP_EXE: &str = "aemeath.exe";
/// 引擎是控制台程序：不为它创建控制台窗口
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// 驱动不可用时的退出码（便于安装程序和脚本区分）
const EXIT_DRIVER_UNAVAILABLE: u8 = 2;

fn main() -> ExitCode {
    let config = match RuntimeConfig::from_args(std::env::args().skip(1)) {
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
    info!(version = env!("CARGO_PKG_VERSION"), data_dir = %config.data_dir.display(), "guardian starting");

    let _instance = match single_instance(&config) {
        Ok(Some(instance)) => instance,
        Ok(None) => {
            info!("another guardian is already running, exiting");
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            error!(%error, "cannot create the single-instance mutex");
            return ExitCode::FAILURE;
        }
    };

    if let Err(problem) = check_driver(&config) {
        error!(?problem, "usbip-win2 is not usable, not starting");
        let preferences = ConfigStore::new(&config.data_dir).peek_preferences();
        let language = messages::pick_language(
            preferences.as_ref().map(|preferences| &preferences.language),
            &ui_language::preferred_ui_languages(),
        );
        if let Err(error) = task_dialog::show_error(&messages::dialog(&problem, language)) {
            error!(%error, "cannot show the driver dialog");
        }
        return ExitCode::from(EXIT_DRIVER_UNAVAILABLE);
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

fn single_instance(config: &RuntimeConfig) -> io::Result<Option<SingleInstance>> {
    SingleInstance::acquire(&config.identity.guardian_instance_name()?)
}

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

/// 引擎和主程序收到与守护进程相同的配置
fn forwarded_arguments(config: &RuntimeConfig) -> Vec<OsString> {
    vec![
        "--data-dir".into(),
        config.data_dir.clone().into_os_string(),
        "--usbip-port".into(),
        config.usbip_port.to_string().into(),
        "--pipe".into(),
        config.pipe_name.clone().into(),
        "--meters".into(),
        config.meter_memory_name.clone().into(),
    ]
}

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
