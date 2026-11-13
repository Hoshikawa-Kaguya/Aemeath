//! 进程层面的名称：数据目录、命名管道、互斥体、共享内存、可执行文件名。
//!
//! 按身份区分，开发身份与正式安装互不干扰。几个进程各自计算，结果一致。

use std::io;
use std::path::PathBuf;

use aemeath_usbip::identity::IdentityProfile;

const APP_DATA_VARIABLE: &str = "APPDATA";

/// 守护进程（用户启动的就是它）、引擎、主程序的可执行文件名，三者放在同一目录
pub const GUARDIAN_EXE: &str = "Aemeath.exe";
pub const ENGINE_EXE: &str = "aemeath-host.exe";
pub const APP_EXE: &str = "aemeath-app.exe";
pub const EXECUTABLES: [&str; 3] = [GUARDIAN_EXE, ENGINE_EXE, APP_EXE];

/// 按身份算出的进程层面的名称。
pub trait ProcessNames {
    /// 数据目录：`%APPDATA%\Aemeath`；开发身份用 `%APPDATA%\Aemeath Dev`，与正式安装互不干扰。
    /// 安装版和便携版相同，不提供修改。
    fn default_data_dir(self) -> Option<PathBuf>;
    /// `%APPDATA%` 下的数据目录名
    fn data_folder_name(self) -> &'static str;
    /// 引擎的命名管道名，带上当前用户的 SID：同一台电脑的不同用户各自运行一个引擎，互不冲突。
    fn engine_pipe_name(self) -> io::Result<String>;
    /// 守护进程的单实例互斥体名称（每个用户、每个登录会话只运行一个守护进程）。
    fn guardian_instance_name(self) -> io::Result<String>;
    /// 主程序唤起事件的名称（每个用户、每个登录会话只运行一个主程序），重复启动时用它请已运行的主程序显示窗口。
    fn app_activation_name(self) -> io::Result<String>;
    /// 电平共享内存的名称（`Local\` 命名空间：引擎和主程序在同一个登录会话里），同样带上用户 SID。
    fn meter_memory_name(self) -> io::Result<String>;
}

/// 命名管道、互斥体、共享内存名称的前缀
const fn name_prefix(identity: IdentityProfile) -> &'static str {
    match identity {
        IdentityProfile::Development => "Aemeath.Dev",
        IdentityProfile::Production => "Aemeath",
    }
}

impl ProcessNames for IdentityProfile {
    fn default_data_dir(self) -> Option<PathBuf> {
        let app_data = std::env::var_os(APP_DATA_VARIABLE)?;
        Some(PathBuf::from(app_data).join(self.data_folder_name()))
    }

    fn data_folder_name(self) -> &'static str {
        match self {
            Self::Development => "Aemeath Dev",
            Self::Production => "Aemeath",
        }
    }

    fn engine_pipe_name(self) -> io::Result<String> {
        let sid = aemeath_sys::pipe::current_user_sid()?;
        let product = name_prefix(self);
        Ok(format!(r"\\.\pipe\{product}.Engine.{sid}"))
    }

    fn guardian_instance_name(self) -> io::Result<String> {
        let sid = aemeath_sys::pipe::current_user_sid()?;
        let product = name_prefix(self);
        Ok(format!(r"Local\{product}.Guardian.{sid}"))
    }

    fn app_activation_name(self) -> io::Result<String> {
        let sid = aemeath_sys::pipe::current_user_sid()?;
        let product = name_prefix(self);
        Ok(format!(r"Local\{product}.App.{sid}"))
    }

    fn meter_memory_name(self) -> io::Result<String> {
        let sid = aemeath_sys::pipe::current_user_sid()?;
        let product = name_prefix(self);
        Ok(format!(r"Local\{product}.Meters.{sid}"))
    }
}
