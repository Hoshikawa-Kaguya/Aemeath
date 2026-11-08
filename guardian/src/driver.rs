//! 启动时检查 usbip-win2 驱动（ADR-0011；方法见 `agent_tmp/docs/usbip-win2集成与升级.md` 第 5、7 节）。
//!
//! 先收集观测（注册表、`usbip.exe`、驱动服务、命令行自检），再由纯函数 [`evaluate`] 判定，便于测试。
//! 任何一项不满足都拒绝启动：没有可用的驱动，虚拟设备就不存在，Aemeath 不提供降级模式。

use aemeath_control::usbip_cli::{UsbipCli, UsbipVersion};
use aemeath_sys::registry;
use aemeath_sys::service::{self, ServiceState};
use tracing::{info, warn};

/// usbip-win2 安装程序（Inno Setup）的卸载注册表项
const UNINSTALL_KEY: &str =
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\{199505b0-b93d-4521-a8c7-897818e0205a}_is1";
const DISPLAY_VERSION: &str = "DisplayVersion";
/// 虚拟主控制器的驱动服务；安装后由 PnP 启动，没有挂载设备时也在运行
const DRIVER_SERVICE: &str = "usbip2_ude";

/// 不能启动的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverProblem {
    NotInstalled,
    /// 低于支持的最低版本（包括已知有问题的 0.9.7.8）
    TooOld {
        found: UsbipVersion,
    },
    /// 已安装，但驱动服务没有运行（例如开启内存完整性后无法加载）
    ServiceNotRunning,
    /// 命令行的版本号或输出无法识别：与 Aemeath 不兼容
    Incompatible {
        detail: String,
    },
}

/// 收集到的观测。
#[derive(Debug, Clone)]
pub struct Inspection {
    pub registry_version: Option<String>,
    /// `usbip.exe --version`；为 None 表示找不到 `usbip.exe`
    pub cli_version: Option<Result<UsbipVersion, String>>,
    pub service: Option<ServiceState>,
    /// `usbip.exe port` 自检
    pub self_test: Result<(), String>,
}

/// 判定能否启动；能启动时返回驱动版本。
pub fn evaluate(inspection: &Inspection) -> Result<UsbipVersion, DriverProblem> {
    if inspection.registry_version.is_none() {
        return Err(DriverProblem::NotInstalled);
    }
    let version = match &inspection.cli_version {
        None => return Err(DriverProblem::NotInstalled),
        Some(Err(detail)) => return Err(DriverProblem::Incompatible { detail: detail.clone() }),
        Some(Ok(version)) => *version,
    };
    match inspection.service {
        None => return Err(DriverProblem::NotInstalled),
        Some(ServiceState::Running) => {}
        Some(ServiceState::Stopped | ServiceState::Other) => return Err(DriverProblem::ServiceNotRunning),
    }
    if version < UsbipVersion::MINIMUM {
        return Err(DriverProblem::TooOld { found: version });
    }
    if let Err(detail) = &inspection.self_test {
        return Err(DriverProblem::Incompatible { detail: detail.clone() });
    }
    Ok(version)
}

/// 收集观测并判定。
pub async fn check(cli: Option<&UsbipCli>) -> Result<UsbipVersion, DriverProblem> {
    let registry_version = registry::local_machine_string(UNINSTALL_KEY, DISPLAY_VERSION).unwrap_or_else(|error| {
        warn!(%error, "cannot read the usbip-win2 uninstall key");
        None
    });
    let (cli_version, self_test) = match cli {
        Some(cli) => (
            Some(cli.version().await.map_err(|error| error.to_string())),
            cli.self_test().await.map_err(|error| error.to_string()),
        ),
        None => (None, Ok(())),
    };
    let service = service::service_state(DRIVER_SERVICE).unwrap_or_else(|error| {
        warn!(%error, "cannot query the usbip-win2 driver service");
        None
    });
    let inspection = Inspection {
        registry_version,
        cli_version,
        service,
        self_test,
    };
    info!(?inspection, "usbip-win2 inspected");
    // 注册表与命令行的版本不一致时以命令行为准（集成文档第 5 节），只记日志
    if let (Some(registered), Some(Ok(cli_version))) = (&inspection.registry_version, &inspection.cli_version)
        && registered.parse::<UsbipVersion>().ok() != Some(*cli_version)
    {
        warn!(%registered, %cli_version, "the registered usbip-win2 version differs from usbip.exe");
    }
    let result = evaluate(&inspection);
    if let Ok(version) = result
        && version > UsbipVersion::BUNDLED
    {
        warn!(%version, bundled = %UsbipVersion::BUNDLED, "usbip-win2 is newer than the tested version");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> UsbipVersion {
        text.parse().unwrap()
    }

    fn healthy() -> Inspection {
        Inspection {
            registry_version: Some("0.9.8.1".to_owned()),
            cli_version: Some(Ok(version("0.9.8.1"))),
            service: Some(ServiceState::Running),
            self_test: Ok(()),
        }
    }

    #[test]
    fn a_healthy_installation_passes() {
        assert_eq!(evaluate(&healthy()), Ok(UsbipVersion::BUNDLED));
        let newer = Inspection {
            cli_version: Some(Ok(version("0.9.9.0"))),
            ..healthy()
        };
        assert_eq!(
            evaluate(&newer),
            Ok(version("0.9.9.0")),
            "newer versions are not downgraded"
        );
    }

    #[test]
    fn a_missing_driver_is_not_installed() {
        let no_key = Inspection {
            registry_version: None,
            ..healthy()
        };
        assert_eq!(evaluate(&no_key), Err(DriverProblem::NotInstalled));
        let no_cli = Inspection {
            cli_version: None,
            ..healthy()
        };
        assert_eq!(evaluate(&no_cli), Err(DriverProblem::NotInstalled));
        let no_service = Inspection {
            service: None,
            ..healthy()
        };
        assert_eq!(evaluate(&no_service), Err(DriverProblem::NotInstalled));
    }

    #[test]
    fn old_and_known_broken_versions_are_too_old() {
        let broken = Inspection {
            cli_version: Some(Ok(UsbipVersion::KNOWN_BROKEN)),
            ..healthy()
        };
        assert_eq!(
            evaluate(&broken),
            Err(DriverProblem::TooOld {
                found: UsbipVersion::KNOWN_BROKEN
            })
        );
        let minimum = Inspection {
            cli_version: Some(Ok(UsbipVersion::MINIMUM)),
            ..healthy()
        };
        assert!(evaluate(&minimum).is_ok());
    }

    #[test]
    fn a_stopped_service_or_unknown_output_refuses_to_start() {
        let stopped = Inspection {
            service: Some(ServiceState::Stopped),
            ..healthy()
        };
        assert_eq!(evaluate(&stopped), Err(DriverProblem::ServiceNotRunning));
        let garbled = Inspection {
            self_test: Err("unrecognized".to_owned()),
            ..healthy()
        };
        assert!(matches!(evaluate(&garbled), Err(DriverProblem::Incompatible { .. })));
        let bad_version = Inspection {
            cli_version: Some(Err("not a version".to_owned())),
            ..healthy()
        };
        assert!(matches!(
            evaluate(&bad_version),
            Err(DriverProblem::Incompatible { .. })
        ));
    }
}
