//! 提权的守护进程实例（M4 计划 G2、W2、W3）：由普通实例经 UAC 启动，做完一件需要管理员权限的事就退出。
//! 这些模式不抢单实例，也不拉起子进程。

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use aemeath_control::crash_dumps;
use aemeath_control::slots::uninstall;
use aemeath_control::usbip_cli::UsbipVersion;
use aemeath_runtime::logging::{self, LOG_FOLDER, Process};
use aemeath_usbip::identity::IdentityProfile;
use sha2::{Digest, Sha256};
use tracing::{error, info};

pub const INSTALL_DRIVER_ARGUMENT: &str = "--elevated-install-driver";
/// 随包附带的 usbip-win2 安装程序的 SHA256（`usbip-win2集成与升级.md` 第 1 节）。
/// 程序目录普通用户可写，而这个文件要以管理员身份运行，运行前必须校验（G2）
const INSTALLER_SHA256: &str = "38cad6d4432b52d5bb9409d9ad03b72fdffc4ada4cd3a48fbeca1a2752a8518a";
const REDIST_FOLDER: &str = "redist";
const INSTALLER_LOG: &str = "usbip-install.log";
/// 退出码：与安装程序自己的退出码（Inno Setup 用 0–8）错开
pub const EXIT_BAD_INSTALLER: u8 = 90;
pub const EXIT_CANNOT_START_INSTALLER: u8 = 91;
pub const EXIT_CANNOT_WRITE_REGISTRY: u8 = 92;

pub fn installer_path(directory: &Path) -> PathBuf {
    directory
        .join(REDIST_FOLDER)
        .join(format!("USBip-{}-x64.exe", UsbipVersion::BUNDLED))
}

pub fn installer_log(data_dir: &Path) -> PathBuf {
    data_dir.join(LOG_FOLDER).join(INSTALLER_LOG)
}

/// 命令行参数是提权模式时执行并返回退出码；否则返回 None。
pub fn dispatch(args: &[String]) -> Option<ExitCode> {
    let identity = IdentityProfile::current();
    match args {
        [mode, data_dir] if mode == INSTALL_DRIVER_ARGUMENT => Some(install_driver(identity, Path::new(data_dir))),
        [mode] if mode == crash_dumps::ENABLE_ARGUMENT => Some(enable_crash_dumps(identity)),
        [mode] if mode == uninstall::REMOVE_ARGUMENT => Some(crate::cleanup::run(identity)),
        _ => None,
    }
}

fn enable_crash_dumps(identity: IdentityProfile) -> ExitCode {
    match crash_dumps::enable(identity) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(EXIT_CANNOT_WRITE_REGISTRY),
    }
}

fn install_driver(identity: IdentityProfile, data_dir: &Path) -> ExitCode {
    // 失败路径的日志必须完整：静默安装的实际表现只能在虚拟机里第一次真实运行（M4 计划 2.6 节）
    let _logs = logging::init(data_dir, Process::Guardian);
    let code = run_installer(data_dir);
    info!(code, "the usbip-win2 installer finished");
    if code == 0
        && let Err(error) = crash_dumps::enable(identity)
    {
        error!(%error, "cannot enable crash dumps");
    }
    ExitCode::from(code)
}

fn run_installer(data_dir: &Path) -> u8 {
    let directory = match std::env::current_exe() {
        Ok(path) => path.parent().map(Path::to_path_buf).unwrap_or_default(),
        Err(error) => {
            error!(%error, "cannot locate the guardian executable");
            return EXIT_BAD_INSTALLER;
        }
    };
    let installer = installer_path(&directory);
    match std::fs::read(&installer) {
        Ok(bytes) if matches_checksum(&bytes) => {}
        Ok(_) => {
            error!(installer = %installer.display(), "the installer does not match the expected SHA256, not running it");
            return EXIT_BAD_INSTALLER;
        }
        Err(error) => {
            error!(%error, installer = %installer.display(), "cannot read the installer");
            return EXIT_BAD_INSTALLER;
        }
    }
    let log = installer_log(data_dir);
    info!(installer = %installer.display(), log = %log.display(), "running the usbip-win2 installer");
    let status = Command::new(&installer)
        .args([
            "/VERYSILENT",
            "/SUPPRESSMSGBOXES",
            "/NORESTART",
            "/SP-",
            "/COMPONENTS=main,client",
            "/TASKS=vcredist",
        ])
        .arg(format!("/LOG={}", log.display()))
        .status();
    match status {
        Ok(status) => u8::try_from(status.code().unwrap_or(i32::from(EXIT_CANNOT_START_INSTALLER)))
            .unwrap_or(EXIT_CANNOT_START_INSTALLER),
        Err(error) => {
            error!(%error, "cannot start the installer");
            EXIT_CANNOT_START_INSTALLER
        }
    }
}

fn matches_checksum(bytes: &[u8]) -> bool {
    let digest = Sha256::digest(bytes);
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    hex == INSTALLER_SHA256
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_the_installer_digest() {
        assert!(!matches_checksum(b"not the installer"));
        assert_eq!(INSTALLER_SHA256.len(), 64);
    }

    #[test]
    fn the_installer_name_follows_the_bundled_version() {
        assert_eq!(
            installer_path(Path::new(r"C:\app")),
            PathBuf::from(r"C:\app\redist\USBip-0.9.8.1-x64.exe")
        );
    }

    #[test]
    fn only_the_exact_arguments_select_an_elevated_mode() {
        assert!(dispatch(&["--data-dir".to_owned(), "x".to_owned()]).is_none());
        assert!(dispatch(&[]).is_none());
        assert!(dispatch(&[INSTALL_DRIVER_ARGUMENT.to_owned()]).is_none());
    }
}
