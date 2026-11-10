//! 开机自启动：当前用户的 Run 键（不需要管理员），指向守护进程。键值名按身份区分，开发构建不影响正式安装。

use std::io;
use std::path::Path;

use aemeath_sys::registry;
use aemeath_usbip::identity::IdentityProfile;

/// Run 键里的命令行带上这个参数：开机时只留在托盘，不弹出主窗口
pub const ARGUMENT: &str = "--autostart";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// 用户在任务管理器里禁用启动项时，Windows 把记录写在这里
const STARTUP_APPROVED_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

fn value_name() -> &'static str {
    IdentityProfile::current().data_folder_name()
}

fn command_line(guardian: &Path) -> String {
    format!("\"{}\" {ARGUMENT}", guardian.display())
}

/// 记录的路径和当前程序一致才算开启：便携版挪过目录后显示关闭，重新打开即可更新路径。
///
/// shortcut: 只看 Run 键，在任务管理器里禁用后这里仍显示开启（重新打开开关可恢复），需要时再读 StartupApproved。
pub fn is_enabled(guardian: &Path) -> io::Result<bool> {
    let stored = registry::current_user_string(RUN_KEY, value_name())?;
    Ok(stored.as_deref() == Some(command_line(guardian).as_str()))
}

pub fn enable(guardian: &Path) -> io::Result<()> {
    registry::set_current_user_string(RUN_KEY, value_name(), &command_line(guardian))?;
    // 在任务管理器里禁用过的话，不删这条记录，Run 键写了也不生效
    registry::delete_current_user_value(STARTUP_APPROVED_KEY, value_name())
}

/// 不存在时视为成功。
pub fn disable() -> io::Result<()> {
    registry::delete_current_user_value(RUN_KEY, value_name())?;
    registry::delete_current_user_value(STARTUP_APPROVED_KEY, value_name())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_path_is_quoted_because_it_may_contain_spaces() {
        assert_eq!(
            command_line(Path::new(r"C:\Program Files\Aemeath\current\Aemeath.exe")),
            r#""C:\Program Files\Aemeath\current\Aemeath.exe" --autostart"#
        );
    }
}
