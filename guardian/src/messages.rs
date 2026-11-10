//! 驱动不可用时对话框的文案（中文、英文）。守护进程没有界面语言包，文案写在这里。
//!
//! 未安装或版本过旧时引导安装附带的驱动（ADR-0011，M4 计划 G1–G5）；其余情况只说明原因。

use aemeath_control::usbip_cli::UsbipVersion;
use aemeath_core::preferences::LanguagePreference;
use aemeath_sys::task_dialog::{ChoiceDialog, ErrorDialog, ProgressDialog};

use std::path::Path;

use crate::driver::DriverProblem;

const TITLE: &str = "Aemeath";
const CHINESE_PREFIX: &str = "zh";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Chinese,
    English,
}

/// 配置里固定了语言时用它；否则按系统界面语言的第一项。
pub fn pick_language(preference: Option<&LanguagePreference>, system: &[String]) -> Language {
    let tag = match preference {
        Some(LanguagePreference::Fixed { locale }) => Some(locale.as_str()),
        Some(LanguagePreference::System) | None => system.first().map(String::as_str),
    };
    if tag.is_some_and(|tag| tag.starts_with(CHINESE_PREFIX)) {
        Language::Chinese
    } else {
        Language::English
    }
}

pub fn dialog(problem: &DriverProblem, language: Language) -> ErrorDialog {
    let bundled = UsbipVersion::BUNDLED;
    let minimum = UsbipVersion::MINIMUM;
    let (heading, content, footer) = match (problem, language) {
        (DriverProblem::NotInstalled, Language::Chinese) => (
            "Aemeath 需要 usbip-win2 驱动".to_owned(),
            format!(
                "Aemeath 的虚拟设备基于 usbip-win2 驱动实现，这台电脑上还没有安装它。\n\n\
                 找不到随附的安装程序。请从 usbip-win2 的官方发布页安装 {bundled} 版，然后重新打开 Aemeath。"
            ),
            "https://github.com/vadimgrn/usbip-win2/releases".to_owned(),
        ),
        (DriverProblem::NotInstalled, Language::English) => (
            "Aemeath needs the usbip-win2 driver".to_owned(),
            format!(
                "Aemeath's virtual devices are built on the usbip-win2 driver, which isn't installed on this PC.\n\n\
                 The bundled installer can't be found. Install usbip-win2 {bundled} from its official \
                 release page, then open Aemeath again."
            ),
            "https://github.com/vadimgrn/usbip-win2/releases".to_owned(),
        ),
        (DriverProblem::TooOld { found }, Language::Chinese) => (
            "usbip-win2 驱动版本过旧".to_owned(),
            format!(
                "这台电脑上安装的是 usbip-win2 {found}，Aemeath 需要 {minimum} 或更新的版本（0.9.7.8 是已知有问题的版本）。\n\n\
                 请安装 {bundled} 版，然后重新打开 Aemeath。"
            ),
            "https://github.com/vadimgrn/usbip-win2/releases".to_owned(),
        ),
        (DriverProblem::TooOld { found }, Language::English) => (
            "The usbip-win2 driver is too old".to_owned(),
            format!(
                "This PC has usbip-win2 {found}. Aemeath needs {minimum} or newer (0.9.7.8 is a known broken version).\n\n\
                 Install version {bundled}, then open Aemeath again."
            ),
            "https://github.com/vadimgrn/usbip-win2/releases".to_owned(),
        ),
        (DriverProblem::ServiceNotRunning, Language::Chinese) => (
            "usbip-win2 驱动无法运行".to_owned(),
            "usbip-win2 已经安装，但它的驱动没有在运行。常见原因是 Windows 安全中心的\u{201c}内存完整性\u{201d}阻止了驱动加载；\
             重新安装通常无法解决。\n\n请重启电脑后再试；如果仍然不行，请查看 Windows 设备管理器中\
             \u{201c}USBip 3.X Emulated Host Controller\u{201d}的状态。"
                .to_owned(),
            String::new(),
        ),
        (DriverProblem::ServiceNotRunning, Language::English) => (
            "The usbip-win2 driver can't run".to_owned(),
            "usbip-win2 is installed, but its driver isn't running. A common cause is Memory Integrity in Windows \
             Security blocking the driver; reinstalling usually doesn't help.\n\nRestart the PC and try again. If it \
             still fails, check \u{201c}USBip 3.X Emulated Host Controller\u{201d} in Device Manager."
                .to_owned(),
            String::new(),
        ),
        (DriverProblem::Incompatible { detail }, Language::Chinese) => (
            "已安装的 usbip-win2 与 Aemeath 不兼容".to_owned(),
            format!(
                "Aemeath 无法识别 usbip-win2 命令行工具的输出，因此无法管理虚拟设备。\n\n\
                 请安装 Aemeath 测试过的 {bundled} 版，然后重新打开 Aemeath。"
            ),
            detail.clone(),
        ),
        (DriverProblem::Incompatible { detail }, Language::English) => (
            "The installed usbip-win2 isn't compatible with Aemeath".to_owned(),
            format!(
                "Aemeath can't understand the output of the usbip-win2 command-line tool, so it can't manage its \
                 virtual devices.\n\nInstall version {bundled}, which Aemeath has been tested with, then open Aemeath again."
            ),
            detail.clone(),
        ),
    };
    ErrorDialog {
        title: TITLE.to_owned(),
        heading,
        content,
        footer,
    }
}

/// 第一个对话框：说明需要安装驱动，提供"安装驱动"和"退出"（G1、G4）。
pub fn install_prompt(problem: &DriverProblem, language: Language) -> ChoiceDialog {
    let bundled = UsbipVersion::BUNDLED;
    let upgrading = matches!(problem, DriverProblem::TooOld { .. });
    let (heading, content, primary, close) = match language {
        Language::Chinese => (
            "Aemeath 需要安装 usbip-win2 驱动",
            if upgrading {
                format!(
                    "Aemeath 的虚拟设备基于 usbip-win2 驱动实现。这台电脑上的版本过旧，需要安装随附的 {bundled} 版。\n\n\
                     这会升级现有的 usbip-win2，其他使用它的软件也会受影响。安装时需要管理员权限。"
                )
            } else {
                format!(
                    "Aemeath 的虚拟设备基于 usbip-win2 驱动实现，这台电脑上还没有安装它。\n\n\
                     将安装随附的 {bundled} 版，需要管理员权限。"
                )
            },
            "安装驱动",
            "退出",
        ),
        Language::English => (
            "Aemeath needs to install the usbip-win2 driver",
            if upgrading {
                format!(
                    "Aemeath's virtual devices are built on the usbip-win2 driver. The version on this PC is too old, so the \
                     bundled version {bundled} needs to be installed.\n\nThis upgrades the existing usbip-win2 and also \
                     affects other software that uses it. Administrator rights are required."
                )
            } else {
                format!(
                    "Aemeath's virtual devices are built on the usbip-win2 driver, which isn't installed on this PC.\n\n\
                     The bundled version {bundled} will be installed. Administrator rights are required."
                )
            },
            "Install driver",
            "Exit",
        ),
    };
    ChoiceDialog {
        title: TITLE.to_owned(),
        heading: heading.to_owned(),
        content,
        footer: String::new(),
        primary: primary.to_owned(),
        close: close.to_owned(),
    }
}

pub fn installing(language: Language) -> ProgressDialog {
    let (heading, content) = match language {
        Language::Chinese => ("正在安装 usbip-win2…", "这可能需要一分钟左右，请不要关闭电脑。"),
        Language::English => (
            "Installing usbip-win2…",
            "This can take about a minute. Don't shut down the PC.",
        ),
    };
    ProgressDialog {
        title: TITLE.to_owned(),
        heading: heading.to_owned(),
        content: content.to_owned(),
    }
}

/// 安装失败：显示退出码和日志路径，按钮是"重试"和"退出"（G5）。
pub fn install_failed(detail: &str, log: &Path, language: Language) -> ChoiceDialog {
    let log = log.display();
    let (heading, content, primary, close) = match language {
        Language::Chinese => (
            "usbip-win2 安装失败",
            format!("安装程序返回：{detail}\n\n详细情况见日志：{log}"),
            "重试",
            "退出",
        ),
        Language::English => (
            "The usbip-win2 installation failed",
            format!("The installer returned: {detail}\n\nSee the log for details: {log}"),
            "Retry",
            "Exit",
        ),
    };
    ChoiceDialog {
        title: TITLE.to_owned(),
        heading: heading.to_owned(),
        content,
        footer: String::new(),
        primary: primary.to_owned(),
        close: close.to_owned(),
    }
}

/// 安装后驱动没有运行：分不清"需要重启"和"真的装不上"，提示重启并附日志路径（G3）。
pub fn restart_needed(log: &Path, language: Language) -> ErrorDialog {
    let log = log.display();
    let (heading, content) = match language {
        Language::Chinese => (
            "请重启电脑后再打开 Aemeath",
            format!(
                "usbip-win2 已经安装，但驱动还没有运行，通常重启后就会正常。\n\n如果重启后仍然不行，请查看日志：{log}"
            ),
        ),
        Language::English => (
            "Restart the PC, then open Aemeath again",
            format!(
                "usbip-win2 is installed, but its driver isn't running yet. A restart usually fixes this.\n\nIf it still fails after a restart, see the log: {log}"
            ),
        ),
    };
    ErrorDialog {
        title: TITLE.to_owned(),
        heading: heading.to_owned(),
        content,
        footer: String::new(),
    }
}

/// 移除虚拟设备时有实例卸载失败（C2）。
pub fn cleanup_failed(failures: usize, log: &Path, language: Language) -> ErrorDialog {
    let log = log.display();
    let (heading, content) = match language {
        Language::Chinese => (
            "部分虚拟设备没有移除",
            format!("有 {failures} 项没有移除成功，这些设备不会影响电脑的使用。\n\n详细情况见日志目录：{log}"),
        ),
        Language::English => (
            "Some virtual devices were not removed",
            format!(
                "{failures} item(s) could not be removed. They do no harm to this PC.\n\nSee the log folder for details: {log}"
            ),
        ),
    };
    ErrorDialog {
        title: TITLE.to_owned(),
        heading: heading.to_owned(),
        content,
        footer: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fixed_preference_wins_over_the_system_language() {
        let fixed = LanguagePreference::Fixed {
            locale: "en".to_owned(),
        };
        let chinese_system = vec!["zh-CN".to_owned()];
        assert_eq!(pick_language(Some(&fixed), &chinese_system), Language::English);
        assert_eq!(
            pick_language(Some(&LanguagePreference::System), &chinese_system),
            Language::Chinese
        );
        assert_eq!(pick_language(None, &["zh-TW".to_owned()]), Language::Chinese);
        assert_eq!(pick_language(None, &[]), Language::English);
    }

    #[test]
    fn every_problem_has_text_in_both_languages() {
        let problems = [
            DriverProblem::NotInstalled,
            DriverProblem::TooOld {
                found: UsbipVersion::KNOWN_BROKEN,
            },
            DriverProblem::ServiceNotRunning,
            DriverProblem::Incompatible { detail: "x".to_owned() },
        ];
        for problem in &problems {
            for language in [Language::Chinese, Language::English] {
                let text = dialog(problem, language);
                assert!(!text.heading.is_empty() && !text.content.is_empty());
            }
        }
    }

    #[test]
    fn the_upgrade_prompt_warns_about_other_software() {
        let old = DriverProblem::TooOld {
            found: UsbipVersion::KNOWN_BROKEN,
        };
        assert!(
            install_prompt(&old, Language::Chinese)
                .content
                .contains("其他使用它的软件")
        );
        assert!(
            install_prompt(&old, Language::English)
                .content
                .contains("other software")
        );
        assert!(
            !install_prompt(&DriverProblem::NotInstalled, Language::English)
                .content
                .contains("other software")
        );
    }

    #[test]
    fn failure_dialogs_include_the_log_path() {
        let log = Path::new(r"C:\data\logs\usbip-install.log");
        assert!(
            install_failed("5", log, Language::English)
                .content
                .contains("usbip-install.log")
        );
        assert!(
            restart_needed(log, Language::Chinese)
                .content
                .contains("usbip-install.log")
        );
    }
}
