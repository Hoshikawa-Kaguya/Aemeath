//! 主程序的偏好设置：与工作台一起保存在配置文件里（[`crate::store`]）。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 界面语言。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum LanguagePreference {
    /// 跟随系统语言
    #[default]
    System,
    /// 固定为某种语言；`locale` 是界面语言包的名称（如 `zh-CN`），由界面解释，后端只保存
    Fixed { locale: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    #[serde(default)]
    pub language: LanguagePreference,
    /// 16 个槽位是否已完成首次注册（ADR-0009）。完成前每次启动都会重新注册。
    #[serde(default)]
    pub slots_registered: bool,
    /// Aemeath 做过的按应用指派：可执行文件的完整路径 → 播放端点 ID（选型报告 5.5 节）。
    /// Windows 按可执行文件持久保存指派，撤销时要求该程序正有音频会话，所以记下来，以便日后撤销。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub app_landings: BTreeMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_preference_has_a_stable_format() {
        let fixed = LanguagePreference::Fixed {
            locale: "zh-CN".to_owned(),
        };
        assert_eq!(
            serde_json::to_string(&fixed).unwrap(),
            r#"{"mode":"fixed","locale":"zh-CN"}"#
        );
        assert_eq!(
            serde_json::to_string(&LanguagePreference::System).unwrap(),
            r#"{"mode":"system"}"#
        );
    }

    #[test]
    fn missing_fields_take_their_defaults() {
        let preferences: Preferences = serde_json::from_str("{}").unwrap();
        assert_eq!(preferences, Preferences::default());
        assert!(!preferences.slots_registered);
    }
}
