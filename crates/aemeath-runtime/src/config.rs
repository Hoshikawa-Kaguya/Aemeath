//! 三个进程（引擎、主程序、守护进程）共用的配置：数据目录、USB/IP 端口、命名管道与电平共享内存的名称。
//!
//! 正式启动时由守护进程传入参数，引擎和主程序拿到的是同一组值；开发时可以不带参数直接运行，
//! 两边按身份算出的默认值一致。

use std::io;
use std::path::PathBuf;

use aemeath_usbip::identity::IdentityProfile;
pub use aemeath_usbip::identity::{DEFAULT_USBIP_PORT, DEV_USBIP_PORT};
use thiserror::Error;

use crate::autostart::ARGUMENT;
use crate::names::ProcessNames;

/// 命名管道名的参数。守护进程拉起引擎和主程序时总会带上它，主程序据此判断自己是不是被直接启动的。
pub const PIPE_ARGUMENT: &str = "--pipe";
/// 开发身份下覆盖 USB/IP 服务端口的环境变量名（正式身份忽略；优先级低于 `--usbip-port`）。
pub const DEV_USBIP_PORT_ENV: &str = "AEMEATH_USBIP_PORT";

/// 三个进程共用的运行参数。守护进程把同一组值传给引擎和主程序，两边才能找到对方。
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// 数据目录：日志在 `logs\`，拓扑缓存在 `engine\`（安装版和便携版都是 `%APPDATA%\Aemeath`）
    pub data_dir: PathBuf,
    /// USB/IP 端口；经守护进程启动时由它决定，直接运行引擎时未明确指定且端口不可用会改由系统分配
    pub usbip_port: u16,
    /// 端口是否由命令行 `--usbip-port` 或开发态环境变量明确指定
    pub usbip_port_explicit: bool,
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
    ///
    /// 开发身份未传 `--usbip-port` 时，还会读取环境变量 [`DEV_USBIP_PORT_ENV`] 覆盖默认端口。
    pub fn from_args(args: impl IntoIterator<Item = String>) -> Result<Self, ConfigError> {
        let env_port = std::env::var(DEV_USBIP_PORT_ENV).ok();
        Self::from_parts(IdentityProfile::current(), args, env_port.as_deref())
    }

    fn from_parts(
        identity: IdentityProfile,
        args: impl IntoIterator<Item = String>,
        dev_port_env: Option<&str>,
    ) -> Result<Self, ConfigError> {
        let mut data_dir = None;
        let mut cli_port = None;
        let mut pipe_name = None;
        let mut meter_memory_name = None;
        let mut autostart = false;
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            let mut value = || args.next().ok_or_else(|| ConfigError::MissingValue(arg.clone()));
            match arg.as_str() {
                "--data-dir" => data_dir = Some(PathBuf::from(value()?)),
                "--usbip-port" => {
                    cli_port = Some(value()?.parse().map_err(|_| ConfigError::InvalidValue(arg.clone()))?);
                }
                PIPE_ARGUMENT => pipe_name = Some(value()?),
                "--meters" => meter_memory_name = Some(value()?),
                ARGUMENT => autostart = true,
                _ => return Err(ConfigError::UnknownArgument(arg)),
            }
        }
        let (usbip_port, usbip_port_explicit) = resolve_port(identity, cli_port, dev_port_env)?;
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
            usbip_port_explicit,
            identity,
            pipe_name,
            meter_memory_name,
            autostart,
        })
    }
}

/// 确定 USB/IP 端口以及它是否来自显式指定：命令行优先，其次是开发身份的环境变量，最后是该身份的默认端口。
fn resolve_port(
    identity: IdentityProfile,
    cli_port: Option<u16>,
    dev_port_env: Option<&str>,
) -> Result<(u16, bool), ConfigError> {
    if let Some(port) = cli_port {
        return Ok((port, true));
    }
    if identity == IdentityProfile::Development
        && let Some(raw) = dev_port_env.map(str::trim).filter(|value| !value.is_empty())
    {
        let port = raw
            .parse()
            .map_err(|_| ConfigError::InvalidValue(DEV_USBIP_PORT_ENV.to_owned()))?;
        return Ok((port, true));
    }
    Ok((identity.default_usbip_port(), false))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn parses_explicit_arguments() {
        let config = RuntimeConfig::from_parts(
            IdentityProfile::Development,
            args(&[
                "--data-dir",
                r"C:\data",
                "--usbip-port",
                "3300",
                "--pipe",
                r"\\.\pipe\test",
            ]),
            None,
        )
        .unwrap();
        assert_eq!(config.data_dir, PathBuf::from(r"C:\data"));
        assert_eq!(config.usbip_port, 3300);
        assert!(config.usbip_port_explicit);
        assert_eq!(config.pipe_name, r"\\.\pipe\test");
    }

    #[test]
    fn only_the_autostart_launch_stays_in_the_tray() {
        assert!(
            RuntimeConfig::from_parts(IdentityProfile::Development, args(&["--autostart"]), None)
                .unwrap()
                .autostart
        );
        assert!(
            !RuntimeConfig::from_parts(IdentityProfile::Development, Vec::new(), None)
                .unwrap()
                .autostart
        );
    }

    #[test]
    fn defaults_include_the_user_sid() {
        let config = RuntimeConfig::from_parts(IdentityProfile::Development, Vec::new(), None).unwrap();
        assert_eq!(config.usbip_port, DEV_USBIP_PORT);
        assert!(!config.usbip_port_explicit);
        assert!(
            config.pipe_name.starts_with(r"\\.\pipe\Aemeath.Dev.Engine.S-1-5-"),
            "{}",
            config.pipe_name
        );
        assert!(config.data_dir.ends_with("Aemeath Dev"));
    }

    #[test]
    fn dev_env_overrides_default_port_only_in_development() {
        let dev = RuntimeConfig::from_parts(IdentityProfile::Development, Vec::new(), Some(" 34567 ")).unwrap();
        assert_eq!(dev.usbip_port, 34567);
        assert!(dev.usbip_port_explicit);

        let dev_ephemeral = RuntimeConfig::from_parts(IdentityProfile::Development, Vec::new(), Some("0")).unwrap();
        assert_eq!(dev_ephemeral.usbip_port, 0);
        assert!(dev_ephemeral.usbip_port_explicit);

        let dev_blank = RuntimeConfig::from_parts(IdentityProfile::Development, Vec::new(), Some("   ")).unwrap();
        assert_eq!(dev_blank.usbip_port, DEV_USBIP_PORT);
        assert!(!dev_blank.usbip_port_explicit);

        // 命令行优先级高于环境变量，即使环境变量是非法值也不受影响
        let cli_wins = RuntimeConfig::from_parts(
            IdentityProfile::Development,
            args(&["--usbip-port", "35000"]),
            Some("not-a-port"),
        )
        .unwrap();
        assert_eq!(cli_wins.usbip_port, 35000);
        assert!(cli_wins.usbip_port_explicit);

        // 正式身份不读取开发环境变量
        let prod = RuntimeConfig::from_parts(IdentityProfile::Production, Vec::new(), Some("34567")).unwrap();
        assert_eq!(prod.usbip_port, DEFAULT_USBIP_PORT);
        assert!(!prod.usbip_port_explicit);

        let prod_bad_env =
            RuntimeConfig::from_parts(IdentityProfile::Production, Vec::new(), Some("not-a-port")).unwrap();
        assert_eq!(prod_bad_env.usbip_port, DEFAULT_USBIP_PORT);
        assert!(!prod_bad_env.usbip_port_explicit);
    }

    #[test]
    fn rejects_bad_arguments() {
        assert!(matches!(
            RuntimeConfig::from_parts(IdentityProfile::Development, args(&["--verbose"]), None),
            Err(ConfigError::UnknownArgument(_))
        ));
        assert!(matches!(
            RuntimeConfig::from_parts(IdentityProfile::Development, args(&["--usbip-port"]), None),
            Err(ConfigError::MissingValue(_))
        ));
        assert!(matches!(
            RuntimeConfig::from_parts(IdentityProfile::Development, args(&["--usbip-port", "x"]), None),
            Err(ConfigError::InvalidValue(_))
        ));
        assert!(matches!(
            RuntimeConfig::from_parts(IdentityProfile::Development, Vec::new(), Some("not-a-port")),
            Err(ConfigError::InvalidValue(name)) if name == DEV_USBIP_PORT_ENV
        ));
    }
}
