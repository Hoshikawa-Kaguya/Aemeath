//! 主程序的偏好设置：与工作台一起保存在配置文件里（[`crate::store`]）。

use std::collections::BTreeMap;

use aemeath_protocol::ids::EndpointId;
use aemeath_protocol::topology::RecorderSettings;
use serde::{Deserialize, Serialize};

use crate::hotkeys::GlobalHotkeys;

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

/// 与路由无关的偏好设置，和工作台存在同一个配置文件里。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    #[serde(default)]
    pub language: LanguagePreference,
    /// 关闭"启动时检查更新"。写成否定形式，默认值就是开启
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_update_check_on_start: bool,
    /// Aemeath 做过的按应用指派：可执行文件的完整路径 → 记录。
    /// Windows 按可执行文件持久保存指派，读写都要求该程序正有音频会话，所以记下来，等它下次出现时纠正或撤销。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub app_landings: BTreeMap<String, LandingRecord>,
    /// 主窗口上次的位置和大小
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowPlacement>,
    /// 上次停留的页面，由界面解释，后端只保存
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_page: Option<String>,
    /// 录音文件夹；None 表示默认的 `%USERPROFILE%\\Music\\Aemeath`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording_folder: Option<String>,
    /// 新录音节点的默认格式和位深
    #[serde(default)]
    pub recording_defaults: RecorderSettings,
    /// 全局热键（GLOSSARY：全局热键）
    #[serde(default)]
    pub global_hotkeys: GlobalHotkeys,
    /// 深浅色、主题色和背景图
    #[serde(default)]
    pub appearance: Appearance,
}

/// 深浅色模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    #[default]
    System,
    Dark,
    Light,
}

/// 界面外观。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Appearance {
    pub theme_mode: ThemeMode,
    /// 主题色 `#rrggbb`；None 表示默认的 Fluent 蓝
    pub accent: Option<String>,
    /// 数据目录 `background\` 下的文件名；None 表示没有背景图
    pub background_file: Option<String>,
    /// 背景遮罩的强度，0–[`Self::MAX_OVERLAY_PERCENT`]
    pub overlay_percent: u8,
}

impl Appearance {
    pub const MAX_OVERLAY_PERCENT: u8 = 90;
    const DEFAULT_OVERLAY_PERCENT: u8 = 60;
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme_mode: ThemeMode::default(),
            accent: None,
            background_file: None,
            overlay_percent: Self::DEFAULT_OVERLAY_PERCENT,
        }
    }
}

/// 一条按应用指派的记录。untagged：指派写成端点 ID 字符串（与早期版本的配置相同，不需要迁移），待清除写成 null。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LandingRecord {
    /// 这个程序应当落在这个端点上
    Endpoint(EndpointId),
    /// 落点节点已被删除，等这个程序下次有音频会话时清除它的指派
    PendingClear,
}

/// 主窗口的位置和大小（物理像素）。`maximized` 时其余字段是还原后的位置和大小。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowPlacement {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub maximized: bool,
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
    fn landing_records_read_the_m2_format() {
        let landings: BTreeMap<String, LandingRecord> =
            serde_json::from_str(r#"{"a.exe":"{id}","b.exe":null}"#).unwrap();
        assert_eq!(
            landings["a.exe"],
            LandingRecord::Endpoint(EndpointId("{id}".to_owned()))
        );
        assert_eq!(landings["b.exe"], LandingRecord::PendingClear);
        assert_eq!(
            serde_json::to_string(&landings).unwrap(),
            r#"{"a.exe":"{id}","b.exe":null}"#
        );
    }

    #[test]
    fn missing_fields_take_their_defaults() {
        let preferences: Preferences = serde_json::from_str("{}").unwrap();
        assert_eq!(preferences, Preferences::default());
        assert_eq!(preferences.appearance.overlay_percent, 60);
    }
}
