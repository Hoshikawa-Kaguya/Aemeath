//! 提权的守护进程实例：由普通实例经 UAC 启动，做完一件需要管理员权限的事就退出。
//! 这些模式不抢单实例，也不拉起子进程。

use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::windows::fs::OpenOptionsExt;
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
/// 程序目录普通用户可写，而这个文件要以管理员身份运行，运行前必须校验
const INSTALLER_SHA256: &str = "38cad6d4432b52d5bb9409d9ad03b72fdffc4ada4cd3a48fbeca1a2752a8518a";
const REDIST_FOLDER: &str = "redist";
const INSTALLER_LOG: &str = "usbip-install.log";
/// 退出码：与安装程序自己的退出码（Inno Setup 用 0–8）错开
pub const EXIT_BAD_INSTALLER: u8 = 90;
pub const EXIT_CANNOT_START_INSTALLER: u8 = 91;
pub const EXIT_CANNOT_WRITE_REGISTRY: u8 = 92;
/// Win32 的 `FILE_SHARE_READ`：别的句柄只能读
const FILE_SHARE_READ: u32 = 0x1;

/// 随包附带的 usbip-win2 安装程序，在程序目录的 `redist\` 下。
pub fn installer_path(directory: &Path) -> PathBuf {
    directory
        .join(REDIST_FOLDER)
        .join(format!("USBip-{}-x64.exe", UsbipVersion::BUNDLED))
}

/// 安装程序自己的日志，与 Aemeath 的日志放在一起，排查安装失败时一并查看。
pub fn installer_log(data_dir: &Path) -> PathBuf {
    data_dir.join(LOG_FOLDER).join(INSTALLER_LOG)
}

/// 命令行参数是提权模式时执行并返回退出码；否则返回 None。
pub fn dispatch(args: &[String]) -> Option<ExitCode> {
    let identity = IdentityProfile::current();
    match args {
        [mode] if mode == INSTALL_DRIVER_ARGUMENT => Some(install_driver(identity)),
        [mode] if mode == crash_dumps::ENABLE_ARGUMENT => Some(enable_crash_dumps(identity)),
        [mode] if mode == uninstall::REMOVE_ARGUMENT => Some(crate::cleanup::run(identity)),
        _ => None,
    }
}

/// 写入崩溃转储的注册表设置（需要管理员权限）。
fn enable_crash_dumps(identity: IdentityProfile) -> ExitCode {
    match crash_dumps::enable(identity) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(EXIT_CANNOT_WRITE_REGISTRY),
    }
}

/// 静默安装附带的驱动；装好后顺便开启崩溃转储，省得再弹一次 UAC。
///
/// 数据目录（日志的位置）自己算，不从命令行接收：提权进程会在那里建目录、删旧日志，不能让别的程序指定。
fn install_driver(identity: IdentityProfile) -> ExitCode {
    let Some(data_dir) = identity.default_data_dir() else {
        return ExitCode::from(EXIT_CANNOT_START_INSTALLER);
    };
    // 静默安装没有界面，在用户电脑上出了问题只能靠日志排查，所以失败路径的日志必须完整
    let _logs = logging::init(&data_dir, Process::Guardian);
    let code = run_installer(&data_dir);
    info!(code, "the usbip-win2 installer finished");
    if code == 0
        && let Err(error) = crash_dumps::enable(identity)
    {
        error!(%error, "cannot enable crash dumps");
    }
    ExitCode::from(code)
}

/// 校验并运行附带的安装程序，返回它的退出码；找不到或校验不通过时返回 `EXIT_BAD_INSTALLER`。
fn run_installer(data_dir: &Path) -> u8 {
    let directory = match std::env::current_exe() {
        Ok(path) => path.parent().map(Path::to_path_buf).unwrap_or_default(),
        Err(error) => {
            error!(%error, "cannot locate the guardian executable");
            return EXIT_BAD_INSTALLER;
        }
    };
    let installer = installer_path(&directory);
    // 句柄一直持有到安装程序退出：校验之后、运行期间文件都不能被改写或替换
    let _locked = match open_locked(&installer) {
        Ok((file, bytes)) if matches_checksum(&bytes) => file,
        Ok(_) => {
            error!(installer = %installer.display(), "the installer does not match the expected SHA256, not running it");
            return EXIT_BAD_INSTALLER;
        }
        Err(error) => {
            error!(%error, installer = %installer.display(), "cannot read the installer");
            return EXIT_BAD_INSTALLER;
        }
    };
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

/// 只允许别人读地打开文件并读出全部内容。返回的句柄持有期间，文件不能被改写、改名或删除。
fn open_locked(path: &Path) -> io::Result<(File, Vec<u8>)> {
    let mut file = OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok((file, bytes))
}

/// 安装程序的 SHA-256 是否与代码里记录的一致。以管理员身份运行它之前，必须确认文件没被替换。
fn matches_checksum(bytes: &[u8]) -> bool {
    let digest = Sha256::digest(bytes);
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    hex == INSTALLER_SHA256
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_locked_installer_cannot_be_replaced() {
        let dir = std::env::temp_dir().join(format!("aemeath-locked-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("installer.exe");
        std::fs::write(&path, b"installer").unwrap();

        let (locked, bytes) = open_locked(&path).unwrap();
        assert_eq!(bytes, b"installer");
        assert!(
            OpenOptions::new().write(true).open(&path).is_err(),
            "cannot be rewritten"
        );
        assert!(
            std::fs::rename(&path, dir.join("moved.exe")).is_err(),
            "cannot be swapped"
        );
        assert!(std::fs::read(&path).is_ok(), "can still be read, so it can run");
        drop(locked);
        assert!(std::fs::rename(&path, dir.join("moved.exe")).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_release_script_bundles_the_same_installer() {
        let script = include_str!("../../scripts/release.ps1");
        assert!(script.contains(&format!("$UsbipVersion = '{}'", UsbipVersion::BUNDLED)));
        assert!(script.contains(&format!("$UsbipSha256 = '{INSTALLER_SHA256}'")));
    }

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
        // 提权模式不接受目录参数：日志的位置由提权进程自己决定
        assert!(dispatch(&[INSTALL_DRIVER_ARGUMENT.to_owned(), r"C:\elsewhere".to_owned()]).is_none());
    }
}
