//! 三个进程（引擎、主程序、守护进程）共用的配置：数据目录、USB/IP 端口、命名管道与电平共享内存的名称。
//!
//! 正式启动时由守护进程传入参数，引擎和主程序拿到的是同一组值；开发时可以不带参数直接运行，
//! 两边按身份算出的默认值一致。

use std::io;
use std::path::PathBuf;

pub use aemeath_usbip::identity::DEFAULT_USBIP_PORT;
use aemeath_usbip::identity::IdentityProfile;
use thiserror::Error;

use crate::autostart::ARGUMENT;

/// 命名管道名的参数。守护进程拉起引擎和主程序时总会带上它，主程序据此判断自己是不是被直接启动的。
pub const PIPE_ARGUMENT: &str = "--pipe";

/// 三个进程共用的运行参数。守护进程把同一组值传给引擎和主程序，两边才能找到对方。
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// 数据目录：日志在 `logs\`，拓扑缓存在 `engine\`（安装版和便携版都是 `%APPDATA%\Aemeath`）
    pub data_dir: PathBuf,
    /// USB/IP 端口；经守护进程启动时由它决定（guardian 的 `usbip_port` 模块）
    pub usbip_port: u16,
    pub identity: IdentityProfile,
    pub pipe_name: String,
    /// 电平共享内存的名称（与主程序共用，见 `aemeath_protocol::meters`）
    pub meter_memory_name: String,
    /// 由开机自启动拉起：主程序只留在托盘，不弹出窗口
    pub autostart: bool,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("unknown argument {0}")]
    UnknownArgument(String),
    #[error("argument {0} needs a value")]
    MissingValue(String),
    #[error("invalid value for {0}")]
    InvalidValue(String),
    #[error("cannot determine the data directory; pass --data-dir")]
    NoDataDir,
    #[error("cannot determine the pipe or shared memory name: {0}")]
    PipeName(io::Error),
}

impl RuntimeConfig {
    /// 解析命令行参数：`--data-dir <目录>`、`--usbip-port <端口>`、`--pipe <管道名>`、
    /// `--meters <共享内存名>`、`--autostart`，都可以省略。
    pub fn from_args(args: impl IntoIterator<Item = String>) -> Result<Self, ConfigError> {
        let identity = IdentityProfile::current();
        let mut data_dir = None;
        let mut usbip_port = DEFAULT_USBIP_PORT;
        let mut pipe_name = None;
        let mut meter_memory_name = None;
        let mut autostart = false;
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            let mut value = || args.next().ok_or_else(|| ConfigError::MissingValue(arg.clone()));
            match arg.as_str() {
                "--data-dir" => data_dir = Some(PathBuf::from(value()?)),
                "--usbip-port" => {
                    usbip_port = value()?.parse().map_err(|_| ConfigError::InvalidValue(arg.clone()))?;
                }
                PIPE_ARGUMENT => pipe_name = Some(value()?),
                "--meters" => meter_memory_name = Some(value()?),
                ARGUMENT => autostart = true,
                _ => return Err(ConfigError::UnknownArgument(arg)),
            }
        }
        let data_dir = match data_dir {
            Some(dir) => dir,
            None => identity.default_data_dir().ok_or(ConfigError::NoDataDir)?,
        };
        let pipe_name = match pipe_name {
            Some(name) => name,
            None => identity.engine_pipe_name().map_err(ConfigError::PipeName)?,
        };
        let meter_memory_name = match meter_memory_name {
            Some(name) => name,
            None => identity.meter_memory_name().map_err(ConfigError::PipeName)?,
        };
        Ok(Self {
            data_dir,
            usbip_port,
            identity,
            pipe_name,
            meter_memory_name,
            autostart,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn parses_explicit_arguments() {
        let config = RuntimeConfig::from_args(args(&[
            "--data-dir",
            r"C:\data",
            "--usbip-port",
            "3300",
            "--pipe",
            r"\\.\pipe\test",
        ]))
        .unwrap();
        assert_eq!(config.data_dir, PathBuf::from(r"C:\data"));
        assert_eq!(config.usbip_port, 3300);
        assert_eq!(config.pipe_name, r"\\.\pipe\test");
    }

    #[test]
    fn only_the_autostart_launch_stays_in_the_tray() {
        assert!(RuntimeConfig::from_args(args(&["--autostart"])).unwrap().autostart);
        assert!(!RuntimeConfig::from_args(Vec::new()).unwrap().autostart);
    }

    #[test]
    fn defaults_include_the_user_sid() {
        let config = RuntimeConfig::from_args(Vec::new()).unwrap();
        assert_eq!(config.usbip_port, DEFAULT_USBIP_PORT);
        assert!(
            config.pipe_name.starts_with(r"\\.\pipe\Aemeath.Dev.Engine.S-1-5-"),
            "{}",
            config.pipe_name
        );
        assert!(config.data_dir.ends_with("Aemeath Dev"));
    }

    #[test]
    fn rejects_bad_arguments() {
        assert!(matches!(
            RuntimeConfig::from_args(args(&["--verbose"])),
            Err(ConfigError::UnknownArgument(_))
        ));
        assert!(matches!(
            RuntimeConfig::from_args(args(&["--usbip-port"])),
            Err(ConfigError::MissingValue(_))
        ));
        assert!(matches!(
            RuntimeConfig::from_args(args(&["--usbip-port", "x"])),
            Err(ConfigError::InvalidValue(_))
        ));
    }
}
