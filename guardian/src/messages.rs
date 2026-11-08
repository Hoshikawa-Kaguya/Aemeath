//! 驱动不可用时对话框的文案（中文、英文）。守护进程没有界面语言包，文案写在这里。
//!
//! M1 只说明原因；引导安装附带的驱动在 M4 加入（ADR-0011）。

use aemeath_control::usbip_cli::UsbipVersion;
use aemeath_core::preferences::LanguagePreference;
use aemeath_sys::task_dialog::ErrorDialog;

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
                 当前版本还不能自动安装驱动。请从 usbip-win2 的官方发布页安装 {bundled} 版，然后重新打开 Aemeath。"
            ),
            "https://github.com/vadimgrn/usbip-win2/releases".to_owned(),
        ),
        (DriverProblem::NotInstalled, Language::English) => (
            "Aemeath needs the usbip-win2 driver".to_owned(),
            format!(
                "Aemeath's virtual devices are built on the usbip-win2 driver, which isn't installed on this PC.\n\n\
                 This version can't install the driver automatically yet. Install usbip-win2 {bundled} from its official \
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
}
