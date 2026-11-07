//! 配置文件：工作台保存为 JSON（ADR-0012：安装版在 `%APPDATA%\Aemeath`，便携版在程序目录下的 `data\`）。
//!
//! - 带结构版本号。遇到更新版本写的配置时拒绝加载，也不覆盖它，避免旧版本把新版本的配置写坏。
//! - 先写临时文件再改名，写入中途进程被结束不会留下半份配置。
//! - 读入后编译成路由拓扑再校验一遍（与引擎收到拓扑时的规则相同）；
//!   文件损坏或不合规时把它改名备份，从空配置开始，并告诉调用方备份的位置。

use std::io;
use std::path::{Path, PathBuf};

use aemeath_protocol::ids::TopologyRevision;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::EngineSettings;
use crate::workspace::Workspace;

/// 配置文件的结构版本。结构有不兼容的变化时加一，并在 [`ConfigStore::load`] 中加入迁移。
pub const CONFIG_VERSION: u32 = 1;
const CONFIG_FILE: &str = "workspace.json";
const TEMP_SUFFIX: &str = "tmp";
const BACKUP_INFIX: &str = "corrupt";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    version: u32,
    workspace: Workspace,
}

/// 只读出版本号，用来在完整解析之前判断是不是更新版本写的配置。
#[derive(Deserialize)]
struct VersionOnly {
    version: u32,
}

/// 读取配置的结果。
#[derive(Debug)]
pub enum Loaded {
    Existing(Workspace),
    /// 还没有配置文件（首次启动）
    Missing,
    /// 配置文件损坏或不合规，已改名备份；返回的是空配置
    Recovered {
        workspace: Workspace,
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

#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    /// `data_dir` 是数据目录（安装版 `%APPDATA%\Aemeath`，便携版 `data\`）。
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join(CONFIG_FILE),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

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
            Ok(workspace) => Ok(Loaded::Existing(workspace)),
            Err(reason) => {
                let backup = self.backup()?;
                Ok(Loaded::Recovered {
                    workspace: Workspace::new(EngineSettings::default()),
                    backup,
                    reason,
                })
            }
        }
    }

    pub fn save(&self, workspace: &Workspace) -> io::Result<()> {
        if let Some(folder) = self.path.parent() {
            std::fs::create_dir_all(folder)?;
        }
        let file = ConfigFile {
            version: CONFIG_VERSION,
            workspace: workspace.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&file).map_err(io::Error::other)?;
        let temp = self.path.with_extension(format!("json.{TEMP_SUFFIX}"));
        std::fs::write(&temp, bytes)?;
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
fn parse(bytes: &[u8]) -> Result<Workspace, String> {
    let file: ConfigFile = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    let workspace = file.workspace;
    if !workspace.ids_are_consistent() {
        return Err("a node ID is not below the next node ID".to_owned());
    }
    workspace
        .compile(TopologyRevision(0))
        .validate()
        .map_err(|error| error.to_string())?;
    Ok(workspace)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NodeName, Position};
    use crate::workspace::NodeSpec;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aemeath-core-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn sample() -> Workspace {
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
        workspace
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
        let Loaded::Recovered { workspace, backup, .. } = store.load().unwrap() else {
            unreachable!("expected recovery")
        };
        assert!(workspace.nodes().is_empty());
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
