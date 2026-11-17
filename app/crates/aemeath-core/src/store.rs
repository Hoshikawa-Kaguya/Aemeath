//! 配置文件：工作台与偏好设置保存为 JSON，放在数据目录（安装版和便携版都是 `%APPDATA%\Aemeath`）。
//!
//! - 带结构版本号。遇到更新版本写的配置时拒绝加载，也不覆盖它，避免旧版本把新版本的配置写坏。
//! - 先写临时文件再改名，写入中途进程被结束不会留下半份配置。
//! - 读入后编译成路由拓扑再校验一遍（与引擎收到拓扑时的规则相同）；
//!   文件损坏或不合规时把它改名备份，从空配置开始，并告诉调用方备份的位置。

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::hotkeys::HotkeyBinding;
use crate::model::EngineSettings;
use crate::preferences::Preferences;
use crate::workspace::Workspace;

/// 配置文件的结构版本。结构有不兼容的变化时加一，并在 [`ConfigStore::load`] 中加入迁移。
///
/// 6：热键去掉"切换/按住"方式，一律是切换。版本 5 的文件由 [`migrate_hotkey_modes`] 迁移。
pub const CONFIG_VERSION: u32 = 6;
/// 能迁移的上一个版本
const PREVIOUS_VERSION: u32 = 5;
/// 配置里存放热键绑定的字段：节点、连线、全局热键
const HOTKEY_FIELDS: [&str; 6] = [
    "mute_hotkey",
    "record_hotkey",
    "toggle_hotkey",
    "all_recording",
    "panic_mute",
    "toggle_window",
];
const CONFIG_FILE: &str = "workspace.json";
const TEMP_SUFFIX: &str = "tmp";
const BACKUP_INFIX: &str = "corrupt";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    version: u32,
    workspace: Workspace,
    #[serde(default)]
    preferences: Preferences,
}

/// 配置文件的内容。
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub workspace: Workspace,
    pub preferences: Preferences,
}

impl Default for Config {
    /// 空工作台（默认引擎设置）与默认偏好
    fn default() -> Self {
        Self {
            workspace: Workspace::new(EngineSettings::default()),
            preferences: Preferences::default(),
        }
    }
}

/// 只读出版本号，用来在完整解析之前判断是不是更新版本写的配置。
#[derive(Deserialize)]
struct VersionOnly {
    version: u32,
}

/// 只读出偏好设置（不校验其余内容）。
#[derive(Deserialize)]
struct PreferencesOnly {
    #[serde(default)]
    preferences: Preferences,
}

/// 读取配置的结果。
#[derive(Debug)]
pub enum Loaded {
    Existing(Config),
    /// 还没有配置文件（首次启动）
    Missing,
    /// 配置文件损坏或不合规，已改名备份；返回的是空配置
    Recovered {
        config: Config,
        backup: PathBuf,
        reason: String,
    },
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error(
        "the configuration was written by a newer version of Aemeath (format {found}, this version reads {CONFIG_VERSION})"
    )]
    NewerVersion { found: u32 },
    #[error("configuration file error: {0}")]
    Io(#[from] io::Error),
}

/// 配置文件 `workspace.json` 的读写。
#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    /// `data_dir` 是数据目录（安装版和便携版都是 `%APPDATA%\Aemeath`）。
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join(CONFIG_FILE),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 读取配置。文件不存在、损坏或不合规都不算错误，结果见 [`Loaded`]；
    /// 只有读不了文件，或文件来自更新的版本时才报错。
    pub fn load(&self) -> Result<Loaded, StoreError> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Loaded::Missing),
            Err(error) => return Err(error.into()),
        };
        if let Ok(VersionOnly { version }) = serde_json::from_slice::<VersionOnly>(&bytes)
            && version > CONFIG_VERSION
        {
            return Err(StoreError::NewerVersion { found: version });
        }
        match parse(&bytes) {
            Ok(config) => Ok(Loaded::Existing(config)),
            Err(reason) => {
                let backup = self.backup()?;
                Ok(Loaded::Recovered {
                    config: Config::default(),
                    backup,
                    reason,
                })
            }
        }
    }

    /// 只读出偏好设置，不校验、也不改动配置文件（守护进程选择对话框语言时用）。读不到时返回 None。
    pub fn peek_preferences(&self) -> Option<Preferences> {
        let bytes = std::fs::read(&self.path).ok()?;
        serde_json::from_slice::<PreferencesOnly>(&bytes)
            .ok()
            .map(|file| file.preferences)
    }

    /// 保存配置：先写临时文件再改名替换。
    pub fn save(&self, config: &Config) -> io::Result<()> {
        if let Some(folder) = self.path.parent() {
            std::fs::create_dir_all(folder)?;
        }
        let file = ConfigFile {
            version: CONFIG_VERSION,
            workspace: config.workspace.clone(),
            preferences: config.preferences.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&file).map_err(io::Error::other)?;
        let temp = self.path.with_extension(format!("json.{TEMP_SUFFIX}"));
        // 改名前先刷盘：否则断电后可能留下内容为零的文件，把原来好的那份也覆盖掉
        let mut written = std::fs::File::create(&temp)?;
        written.write_all(&bytes)?;
        written.sync_all()?;
        drop(written);
        // Windows 上 rename 会替换已存在的目标文件
        std::fs::rename(&temp, &self.path)
    }

    /// 把损坏的配置改名为 `workspace.corrupt-<时间戳>.json` 保留下来，供排查或手工恢复。
    fn backup(&self) -> io::Result<PathBuf> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or_default();
        let backup = self.path.with_extension(format!("{BACKUP_INFIX}-{stamp}.json"));
        std::fs::rename(&self.path, &backup)?;
        Ok(backup)
    }
}

/// 解析并校验配置；返回错误说明（写进日志和提示中）。
fn parse(bytes: &[u8]) -> Result<Config, String> {
    let file: ConfigFile = match serde_json::from_slice::<VersionOnly>(bytes) {
        Ok(VersionOnly {
            version: PREVIOUS_VERSION,
        }) => serde_json::from_slice(bytes).and_then(|mut value| {
            migrate_hotkey_modes(&mut value);
            serde_json::from_value(value)
        }),
        _ => serde_json::from_slice(bytes),
    }
    .map_err(|error| error.to_string())?;
    let workspace = file.workspace;
    workspace.check().map_err(|error| error.to_string())?;
    Ok(Config {
        workspace,
        preferences: file.preferences,
    })
}

/// 版本 5 → 6：去掉热键绑定的 `mode`，按住型改成切换。
/// 不带修饰键的键盘按键以前只能做按住型，不符合切换的规则，删掉这个绑定。
fn migrate_hotkey_modes(file: &mut serde_json::Value) {
    file["version"] = CONFIG_VERSION.into();
    for path in ["/workspace/nodes", "/workspace/routes"] {
        if let Some(owners) = file.pointer_mut(path).and_then(serde_json::Value::as_array_mut) {
            owners.iter_mut().for_each(migrate_owner);
        }
    }
    if let Some(global) = file.pointer_mut("/preferences/global_hotkeys") {
        migrate_owner(global);
    }
}

fn migrate_owner(owner: &mut serde_json::Value) {
    let Some(owner) = owner.as_object_mut() else {
        return;
    };
    for field in HOTKEY_FIELDS {
        let Some(binding) = owner.get_mut(field) else {
            continue;
        };
        if let Some(binding) = binding.as_object_mut() {
            binding.remove("mode");
        }
        // 解析不了的留给随后的整体解析报告为损坏
        if serde_json::from_value::<HotkeyBinding>(binding.clone()).is_ok_and(|binding| !binding.is_valid()) {
            tracing::warn!(
                field,
                "a hold hotkey without a modifier cannot become a toggle, removed"
            );
            owner.remove(field);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkeys::{HotkeyTarget, all_hotkeys};
    use crate::model::{NodeName, Position};
    use crate::workspace::NodeSpec;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aemeath-core-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn sample() -> Config {
        let mut workspace = Workspace::new(EngineSettings::default());
        let game = workspace
            .add_node(
                NodeName::new("游戏").unwrap(),
                NodeSpec::VirtualInput(None),
                Position { x: 10.0, y: 20.0 },
            )
            .unwrap();
        let bus = workspace
            .add_node(NodeName::new("Bus").unwrap(), NodeSpec::Bus, Position::default())
            .unwrap();
        workspace.connect(game, bus).unwrap();
        Config {
            workspace,
            preferences: Preferences {
                language: crate::preferences::LanguagePreference::Fixed {
                    locale: "zh-CN".to_owned(),
                },
                app_landings: std::collections::BTreeMap::from([(
                    r"C:\Games\game.exe".to_owned(),
                    crate::preferences::LandingRecord::Endpoint(aemeath_protocol::ids::EndpointId(
                        "{0.0.0.00000000}.{in1}".to_owned(),
                    )),
                )]),
                window: Some(crate::preferences::WindowPlacement {
                    x: -100,
                    y: 20,
                    width: 1600,
                    height: 900,
                    maximized: true,
                }),
                last_page: Some("devices".to_owned()),
                ..Preferences::default()
            },
        }
    }

    #[test]
    fn missing_file_is_reported() {
        let dir = temp_dir("missing");
        assert!(matches!(ConfigStore::new(&dir).load().unwrap(), Loaded::Missing));
    }

    #[test]
    fn saves_and_loads_the_workspace() {
        let dir = temp_dir("round-trip");
        let store = ConfigStore::new(&dir);
        store.save(&sample()).unwrap();
        let Loaded::Existing(loaded) = store.load().unwrap() else {
            unreachable!("expected an existing configuration")
        };
        assert_eq!(loaded, sample());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_files_are_backed_up() {
        let dir = temp_dir("corrupt");
        let store = ConfigStore::new(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(store.path(), b"{ not json").unwrap();
        let Loaded::Recovered { config, backup, .. } = store.load().unwrap() else {
            unreachable!("expected recovery")
        };
        assert!(config.workspace.nodes().is_empty());
        assert!(backup.exists());
        assert!(!store.path().exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_workspaces_are_backed_up() {
        let dir = temp_dir("invalid");
        let store = ConfigStore::new(&dir);
        store.save(&sample()).unwrap();
        // 手工改坏：两个节点占用同一个槽位
        let text = std::fs::read_to_string(store.path()).unwrap();
        let broken = text.replacen(
            "\"type\": \"bus\"",
            "\"type\": \"virtual_input\", \"slot\": {\"direction\": \"in\", \"number\": 1}",
            1,
        );
        std::fs::write(store.path(), broken).unwrap();
        assert!(matches!(store.load().unwrap(), Loaded::Recovered { .. }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preferences_can_be_peeked_without_touching_the_file() {
        let dir = temp_dir("peek");
        let store = ConfigStore::new(&dir);
        assert_eq!(store.peek_preferences(), None);
        store.save(&sample()).unwrap();
        assert_eq!(store.peek_preferences(), Some(sample().preferences));
        std::fs::write(store.path(), b"{ not json").unwrap();
        assert_eq!(store.peek_preferences(), None);
        assert!(store.path().exists(), "peeking never backs up or renames the file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 升级迁移：版本 5 的热键带 `mode`。迁移后都是切换，不带修饰键的键盘按住型被删掉，其余照常读入
    #[test]
    fn version_5_hotkeys_become_toggles() {
        let dir = temp_dir("v5");
        let store = ConfigStore::new(&dir);
        store.save(&sample()).unwrap();
        let mut file: serde_json::Value = serde_json::from_slice(&std::fs::read(store.path()).unwrap()).unwrap();
        file["version"] = 5.into();
        file["workspace"]["nodes"][0]["mute_hotkey"] =
            serde_json::json!({"key": {"key": {"type": "keyboard", "vk": 0x7C}}, "mode": "hold"});
        file["workspace"]["routes"][0]["toggle_hotkey"] = serde_json::json!(
            {"key": {"modifiers": {"ctrl": true}, "key": {"type": "keyboard", "vk": 0x4D}}, "mode": "hold"}
        );
        file["preferences"]["global_hotkeys"]["panic_mute"] =
            serde_json::json!({"key": {"key": {"type": "mouse_x1"}}, "mode": "toggle"});
        std::fs::write(store.path(), serde_json::to_vec(&file).unwrap()).unwrap();

        let Loaded::Existing(loaded) = store.load().unwrap() else {
            unreachable!("expected the version 5 file to be migrated")
        };
        let targets: Vec<HotkeyTarget> = all_hotkeys(&loaded.workspace, &loaded.preferences)
            .into_iter()
            .map(|(target, _)| target)
            .collect();
        let route = loaded.workspace.routes()[0];
        assert_eq!(
            targets,
            vec![HotkeyTarget::PanicMute, HotkeyTarget::RouteToggle(route.from, route.to)]
        );
        assert_eq!(loaded.workspace.nodes().len(), sample().workspace.nodes().len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn newer_versions_are_left_untouched() {
        let dir = temp_dir("newer");
        let store = ConfigStore::new(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(store.path(), br#"{"version": 99, "workspace": {"future": true}}"#).unwrap();
        assert!(matches!(store.load(), Err(StoreError::NewerVersion { found: 99 })));
        assert!(
            store.path().exists(),
            "the newer file must not be renamed or overwritten"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
