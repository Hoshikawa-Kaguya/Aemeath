//! 虚拟设备清理。
//!
//! 卸载钩子有 30 秒时限，所以钩子只启动提权进程、不等它清理完。但启动本身要等用户回应 UAC
//! （`ShellExecuteEx` 会阻塞到用户点完），这段时间算在时限里。
//! 提权进程跑在临时目录里的副本上：卸载程序随后要删除安装目录，不能占着里面的 exe。

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use aemeath_control::crash_dumps;
use aemeath_control::slots::uninstall::{self, REMOVE_ARGUMENT};
use aemeath_control::usbip_cli::UsbipCli;
use aemeath_runtime::autostart;
use aemeath_runtime::logging::{self, LOG_FOLDER, Process};
use aemeath_sys::elevation::run_elevated;
use aemeath_sys::process::{ProcessEntry, process_snapshot};
use aemeath_sys::{task_dialog, ui_language};
use aemeath_usbip::identity::{DEFAULT_USBIP_PORT, IdentityProfile};
use tracing::{error, info, warn};

use crate::messages;

const WAIT_FOR_EXIT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const OWN_EXECUTABLES: [&str; 3] = ["aemeath.exe", "aemeath-host.exe", "aemeath-app.exe"];
const EXIT_REMOVAL_FAILED: u8 = 93;

/// 卸载钩子：把自己复制到临时目录，提权启动清理，不等清理完。UAC 选"否"就保留设备，卸载继续。
pub fn launch_from_temp() {
    // 在这里删而不是提权进程里：Run 键属于当前用户，提权后可能换成管理员账户的 HKCU
    let _ = autostart::disable();
    let Some(copy) = copy_to_temp() else {
        return;
    };
    // 不等清理完：只关闭句柄。钩子里没有界面可以报错，失败时设备保留，这些实例无害
    let _ = run_elevated(&copy, REMOVE_ARGUMENT);
}

/// shortcut: 临时目录里的 exe 留在原处，由系统清理；提权进程运行时不能删除自己。
fn copy_to_temp() -> Option<PathBuf> {
    let source = std::env::current_exe().ok()?;
    let folder = std::env::temp_dir().join(format!("aemeath-cleanup-{}", std::process::id()));
    std::fs::create_dir_all(&folder).ok()?;
    let target = folder.join(source.file_name()?);
    std::fs::copy(&source, &target).ok()?;
    Some(target)
}

/// 除自己之外还有没有 Aemeath 的进程在运行。
fn others_running(processes: &[ProcessEntry], own_id: u32) -> bool {
    processes.iter().any(|process| {
        process.process_id != own_id
            && OWN_EXECUTABLES
                .iter()
                .any(|name| process.exe_name.eq_ignore_ascii_case(name))
    })
}

/// 等其他 Aemeath 进程退出，最多等一段时间。它们还挂着设备时，卸载会失败或留下残留。
fn wait_for_exit() {
    let own_id = std::process::id();
    let started = Instant::now();
    while started.elapsed() < WAIT_FOR_EXIT {
        match process_snapshot() {
            Ok(processes) if others_running(&processes, own_id) => std::thread::sleep(POLL_INTERVAL),
            Ok(_) => return,
            Err(error) => {
                warn!(%error, "cannot list processes");
                return;
            }
        }
    }
    warn!("Aemeath is still running after the wait, removing the devices anyway");
}

/// 提权进程的主体。
pub fn run(identity: IdentityProfile) -> ExitCode {
    let data_dir = identity.default_data_dir();
    let _logs = data_dir
        .as_deref()
        .and_then(|directory| logging::init(directory, Process::Guardian).ok());
    info!("removing the virtual devices");
    wait_for_exit();
    let failures = remove_devices(identity);
    if let Err(error) = crash_dumps::disable() {
        error!(%error, "cannot remove the crash dump settings");
    }
    if failures == 0 {
        info!("the virtual devices are removed");
        return ExitCode::SUCCESS;
    }
    error!(failures, "some virtual devices could not be removed");
    let log = data_dir.as_deref().map(|directory| directory.join(LOG_FOLDER));
    let language = messages::pick_language(None, &ui_language::preferred_ui_languages());
    let dialog = messages::cleanup_failed(failures, log.as_deref().unwrap_or(Path::new("")), language);
    if let Err(error) = task_dialog::show_error(&dialog) {
        error!(%error, "cannot show the dialog");
    }
    ExitCode::from(EXIT_REMOVAL_FAILED)
}

/// 返回失败的实例数；取不到实例列表时算一次失败。
fn remove_devices(identity: IdentityProfile) -> usize {
    let Some(program) = UsbipCli::default_program() else {
        error!("%ProgramFiles% is not set");
        return 1;
    };
    let cli = UsbipCli::new(program, DEFAULT_USBIP_PORT);
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            error!(%error, "cannot start the async runtime");
            return 1;
        }
    };
    match runtime.block_on(uninstall::detach_and_remove(identity, &cli)) {
        Ok(removals) => {
            for removal in &removals {
                match &removal.result {
                    Ok(()) => info!(instance = %removal.instance.instance_id, "removed"),
                    Err(error) => error!(%error, "cannot remove a device instance"),
                }
            }
            removals.iter().filter(|removal| removal.result.is_err()).count()
        }
        Err(error) => {
            error!(%error, "cannot list the device instances");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(process_id: u32, exe_name: &str) -> ProcessEntry {
        ProcessEntry {
            process_id,
            parent_id: 1,
            exe_name: exe_name.to_owned(),
        }
    }

    #[test]
    fn only_other_aemeath_processes_count() {
        let own = 10;
        assert!(!others_running(
            &[entry(own, "Aemeath.exe"), entry(11, "explorer.exe")],
            own
        ));
        assert!(others_running(
            &[entry(own, "Aemeath.exe"), entry(12, "AEMEATH-HOST.EXE")],
            own
        ));
        assert!(others_running(&[entry(13, "aemeath-app.exe")], own));
    }
}
