//! 驱动不可用时的引导（ADR-0011，M4 计划 G1–G5）：对话框 → UAC → 静默安装 → 再次检查 → 继续启动或提示重启。

use std::path::Path;

use aemeath_runtime::config::RuntimeConfig;
use aemeath_sys::elevation::{ElevateError, run_elevated};
use aemeath_sys::task_dialog;
use tracing::{error, info, warn};

use crate::driver::DriverProblem;
use crate::elevated::{INSTALL_DRIVER_ARGUMENT, installer_log, installer_path};
use crate::messages::{self, Language};

/// 这个问题能否靠运行附带的安装程序解决（G4）。服务无法运行时重新安装也解决不了。
pub fn can_install(problem: &DriverProblem) -> bool {
    matches!(problem, DriverProblem::NotInstalled | DriverProblem::TooOld { .. })
}

/// 安装程序运行完后，再次检查的结果决定下一步（G3）。
#[derive(Debug, PartialEq, Eq)]
pub enum AfterInstall {
    Continue,
    /// 分不清"需要重启"和"真的装不上"，都提示重启，提示里附日志路径
    AskRestart,
    Explain(DriverProblem),
}

pub fn after_install(recheck: Result<(), DriverProblem>) -> AfterInstall {
    match recheck {
        Ok(()) => AfterInstall::Continue,
        Err(DriverProblem::ServiceNotRunning | DriverProblem::NotInstalled) => AfterInstall::AskRestart,
        Err(problem) => AfterInstall::Explain(problem),
    }
}

/// 一次安装尝试的结果。
enum Attempt {
    Installed,
    Declined,
    Failed(String),
}

fn attempt(program: &Path, config: &RuntimeConfig) -> Attempt {
    let arguments = format!("{INSTALL_DRIVER_ARGUMENT} \"{}\"", config.data_dir.display());
    match run_elevated(program, &arguments) {
        Err(ElevateError::Cancelled) => Attempt::Declined,
        Err(error) => Attempt::Failed(error.to_string()),
        Ok(process) => match process.wait() {
            Ok(0) => Attempt::Installed,
            Ok(code) => Attempt::Failed(code.to_string()),
            Err(error) => Attempt::Failed(error.to_string()),
        },
    }
}

/// 尝试解决驱动问题。返回 true 表示可以继续启动。
pub fn resolve(
    config: &RuntimeConfig,
    problem: &DriverProblem,
    language: Language,
    recheck: impl Fn() -> Result<(), DriverProblem>,
) -> bool {
    let program = std::env::current_exe().ok();
    let installer_present = program
        .as_deref()
        .and_then(Path::parent)
        .is_some_and(|directory| installer_path(directory).is_file());
    let Some(program) = program.filter(|_| can_install(problem) && installer_present) else {
        show_error(&messages::dialog(problem, language));
        return false;
    };

    let log = installer_log(&config.data_dir);
    let mut ask_first = true;
    loop {
        if ask_first && !task_dialog::ask(&messages::install_prompt(problem, language)).unwrap_or(false) {
            info!("the user chose not to install the driver");
            return false;
        }
        let result = task_dialog::run_with_progress(&messages::installing(language), || attempt(&program, config))
            .unwrap_or_else(|error| Attempt::Failed(error.to_string()));
        match result {
            Attempt::Installed => {
                return match after_install(recheck()) {
                    AfterInstall::Continue => {
                        info!("the driver is installed, continuing");
                        true
                    }
                    AfterInstall::AskRestart => {
                        warn!("the driver is not running after the installation, asking for a restart");
                        show_error(&messages::restart_needed(&log, language));
                        false
                    }
                    AfterInstall::Explain(problem) => {
                        show_error(&messages::dialog(&problem, language));
                        false
                    }
                };
            }
            // UAC 选"否"回到第一个对话框（G5）
            Attempt::Declined => ask_first = true,
            Attempt::Failed(detail) => {
                error!(%detail, "the driver installation failed");
                if !task_dialog::ask(&messages::install_failed(&detail, &log, language)).unwrap_or(false) {
                    return false;
                }
                ask_first = false;
            }
        }
    }
}

fn show_error(dialog: &task_dialog::ErrorDialog) {
    if let Err(error) = task_dialog::show_error(dialog) {
        error!(%error, "cannot show the dialog");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aemeath_control::usbip_cli::UsbipVersion;

    #[test]
    fn only_a_missing_or_old_driver_is_installed() {
        assert!(can_install(&DriverProblem::NotInstalled));
        assert!(can_install(&DriverProblem::TooOld {
            found: UsbipVersion::KNOWN_BROKEN
        }));
        assert!(!can_install(&DriverProblem::ServiceNotRunning));
        assert!(!can_install(&DriverProblem::Incompatible { detail: "x".to_owned() }));
    }

    #[test]
    fn the_recheck_decides_what_happens_next() {
        assert_eq!(after_install(Ok(())), AfterInstall::Continue);
        assert_eq!(
            after_install(Err(DriverProblem::ServiceNotRunning)),
            AfterInstall::AskRestart
        );
        assert_eq!(
            after_install(Err(DriverProblem::NotInstalled)),
            AfterInstall::AskRestart
        );
        let incompatible = DriverProblem::Incompatible { detail: "x".to_owned() };
        assert_eq!(
            after_install(Err(incompatible.clone())),
            AfterInstall::Explain(incompatible)
        );
    }
}
