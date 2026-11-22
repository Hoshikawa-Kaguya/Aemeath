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

use crate::model::EngineSettings;
use crate::preferences::Preferences;
use crate::workspace::Workspace;

/// 配置文件的结构版本。结构有不兼容的变化时加一，并在 [`ConfigStore::load`] 中加入迁移。
///
/// 7：节点新增 `locked`（默认 false），偏好设置新增 `console_layout`（默认展开、默认高度）。
/// 均为带默认值的可选字段，版本 6 的文件可直接读入，启动时由 [`ConfigStore::upgrade`] 写回成版本 7。
pub const CONFIG_VERSION: u32 = 7;
/// 能迁移的上一个版本
const PREVIOUS_VERSION: u32 = 6;
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

    /// 配置文件是上一个版本时，迁移后立即写回成当前版本，返回是否升级了。
    ///
    /// 启动时在读取任何设置之前调用：[`Self::peek_preferences`] 不做迁移，旧版本的文件可能读不出来。
    /// 迁移后不合规的文件不动，留给 [`Self::load`] 备份。
    pub fn upgrade(&self) -> io::Result<bool> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        if !matches!(
            serde_json::from_slice::<VersionOnly>(&bytes),
            Ok(VersionOnly {
                version: PREVIOUS_VERSION
            })
        ) {
            return Ok(false);
        }
        let Ok(config) = parse(&bytes) else {
            return Ok(false);
        };
        self.save(&config)?;
        Ok(true)
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
    let file: ConfigFile = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    if file.version != CONFIG_VERSION && file.version != PREVIOUS_VERSION {
        return Err(format!(
            "unsupported configuration version {} (expected {} or {})",
            file.version, PREVIOUS_VERSION, CONFIG_VERSION
        ));
    }
    let workspace = file.workspace;
    workspace.check().map_err(|error| error.to_string())?;
    let mut preferences = file.preferences;
    preferences.console_layout = preferences.console_layout.normalized();
    Ok(Config { workspace, preferences })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NodeName, Position};
    use crate::preferences::ConsoleLayout;
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
        workspace.set_node_locked(game, true).unwrap();
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
                console_layout: ConsoleLayout {
                    height: Some(360),
                    collapsed: false,
                },
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

    /// 升级迁移：版本 6 没有 `locked` 和 `console_layout`，直接读入取默认值，启动时写回成版本 7。
    #[test]
    fn version_6_files_are_upgraded_before_anything_reads_them() {
        let dir = temp_dir("v6");
        let store = ConfigStore::new(&dir);
        store.save(&sample()).unwrap();
        let mut file: serde_json::Value = serde_json::from_slice(&std::fs::read(store.path()).unwrap()).unwrap();
        file["version"] = 6.into();
        file["workspace"]["nodes"][0].as_object_mut().unwrap().remove("locked");
        file["preferences"].as_object_mut().unwrap().remove("console_layout");
        std::fs::write(store.path(), serde_json::to_vec(&file).unwrap()).unwrap();

        assert!(store.upgrade().unwrap());
        assert!(!store.upgrade().unwrap(), "already the current version");
        let written: serde_json::Value = serde_json::from_slice(&std::fs::read(store.path()).unwrap()).unwrap();
        assert_eq!(written["version"], CONFIG_VERSION);
        let Loaded::Existing(loaded) = store.load().unwrap() else {
            unreachable!("expected the version 6 file to be migrated")
        };
        assert!(!loaded.workspace.nodes()[0].locked);
        assert_eq!(loaded.preferences.console_layout, ConsoleLayout::default());
        assert_eq!(loaded.workspace.nodes().len(), sample().workspace.nodes().len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn older_versions_beyond_previous_are_backed_up() {
        let dir = temp_dir("v5-old");
        let store = ConfigStore::new(&dir);
        store.save(&sample()).unwrap();
        let mut file: serde_json::Value = serde_json::from_slice(&std::fs::read(store.path()).unwrap()).unwrap();
        file["version"] = 5.into();
        std::fs::write(store.path(), serde_json::to_vec(&file).unwrap()).unwrap();

        assert!(
            !store.upgrade().unwrap(),
            "only the immediately previous version upgrades"
        );
        let Loaded::Recovered { config, backup, .. } = store.load().unwrap() else {
            unreachable!("expected older unsupported version to be backed up")
        };
        assert!(config.workspace.nodes().is_empty());
        assert!(backup.exists());
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

    #[test]
    fn out_of_range_console_height_is_clamped_on_load() {
        let dir = temp_dir("console-clamp");
        let store = ConfigStore::new(&dir);
        for (raw, expected) in [(50u16, ConsoleLayout::MIN_HEIGHT), (5000u16, ConsoleLayout::MAX_HEIGHT)] {
            let mut config = sample();
            config.preferences.console_layout.height = Some(raw);
            store.save(&config).unwrap();
            let Loaded::Existing(loaded) = store.load().unwrap() else {
                unreachable!("expected existing configuration")
            };
            assert_eq!(loaded.preferences.console_layout.height, Some(expected));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
