//! 崩溃转储：Windows 错误报告（WER）的 LocalDumps。
//!
//! 微软文档只写了 `HKLM`，写入需要管理员权限，由提权的守护进程执行；读取不需要。

use std::io;

use aemeath_sys::registry;
use aemeath_usbip::identity::IdentityProfile;

/// 让提权的守护进程写入这些键的命令行参数
pub const ENABLE_ARGUMENT: &str = "--enable-crash-dumps";
const LOCAL_DUMPS_KEY: &str = r"SOFTWARE\Microsoft\Windows\Windows Error Reporting\LocalDumps";
/// 三个进程的文件名
const EXECUTABLES: [&str; 3] = ["Aemeath.exe", "aemeath-host.exe", "aemeath-app.exe"];
const DUMP_FOLDER: &str = "DumpFolder";
const DUMP_COUNT: &str = "DumpCount";
const DUMP_TYPE: &str = "DumpType";
const MAX_DUMPS: u32 = 10;
/// WER 的 DumpType：1 表示 Mini dump
const MINI_DUMP: u32 = 1;

fn key_of(executable: &str) -> String {
    format!(r"{LOCAL_DUMPS_KEY}\{executable}")
}

/// `%APPDATA%\<数据目录名>\crashes`。写成 `REG_EXPAND_SZ`，由 WER 在崩溃进程的用户下展开。
fn dump_folder(identity: IdentityProfile) -> String {
    format!(r"%APPDATA%\{}\crashes", identity.data_folder_name())
}

/// 三个进程的键是否都已写入（不需要管理员）。
pub fn is_enabled() -> io::Result<bool> {
    for executable in EXECUTABLES {
        if !registry::local_machine_key_exists(&key_of(executable))? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// 写入三个进程的键（需要管理员）。
pub fn enable(identity: IdentityProfile) -> io::Result<()> {
    let folder = dump_folder(identity);
    for executable in EXECUTABLES {
        let key = key_of(executable);
        registry::set_local_machine_expand_string(&key, DUMP_FOLDER, &folder)?;
        registry::set_local_machine_dword(&key, DUMP_COUNT, MAX_DUMPS)?;
        registry::set_local_machine_dword(&key, DUMP_TYPE, MINI_DUMP)?;
    }
    Ok(())
}

/// 删除三个进程的键（需要管理员），键不存在时视为成功。
pub fn disable() -> io::Result<()> {
    for executable in EXECUTABLES {
        registry::delete_local_machine_tree(&key_of(executable))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_folder_stays_unexpanded_and_follows_the_identity() {
        assert_eq!(dump_folder(IdentityProfile::Production), r"%APPDATA%\Aemeath\crashes");
        assert_eq!(
            dump_folder(IdentityProfile::Development),
            r"%APPDATA%\Aemeath Dev\crashes"
        );
    }

    #[test]
    fn keys_are_per_executable() {
        assert_eq!(
            key_of("Aemeath.exe"),
            r"SOFTWARE\Microsoft\Windows\Windows Error Reporting\LocalDumps\Aemeath.exe"
        );
    }
}
