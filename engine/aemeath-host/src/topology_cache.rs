//! 最近一份路由拓扑的缓存（ADR-0008：引擎重启后先按缓存恢复出声，再等主程序下发最新拓扑）。
//!
//! 先写临时文件再改名，进程在写入中途被结束时不会留下半份拓扑。读取时重新校验，不信任磁盘上的内容。

use std::io;
use std::path::{Path, PathBuf};

use aemeath_protocol::topology::RouteTopology;

const CACHE_FOLDER: &str = "engine";
const CACHE_FILE: &str = "topology.json";
const TEMP_FILE: &str = "topology.json.tmp";

#[derive(Debug, Clone)]
pub struct TopologyCache {
    folder: PathBuf,
}

impl TopologyCache {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            folder: data_dir.join(CACHE_FOLDER),
        }
    }

    /// 读取缓存的拓扑；没有缓存、读不出来或校验不通过时返回 None（后两种情况记录警告）。
    pub fn load(&self) -> Option<RouteTopology> {
        let path = self.folder.join(CACHE_FILE);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "cannot read the cached topology");
                return None;
            }
        };
        let topology: RouteTopology = match serde_json::from_slice(&bytes) {
            Ok(topology) => topology,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "the cached topology is not valid JSON");
                return None;
            }
        };
        if let Err(error) = topology.validate() {
            tracing::warn!(path = %path.display(), %error, "the cached topology is not valid");
            return None;
        }
        Some(topology)
    }

    pub fn save(&self, topology: &RouteTopology) -> io::Result<()> {
        std::fs::create_dir_all(&self.folder)?;
        let temp = self.folder.join(TEMP_FILE);
        let bytes = serde_json::to_vec_pretty(topology).map_err(io::Error::other)?;
        std::fs::write(&temp, bytes)?;
        // Windows 上 rename 会替换已存在的目标文件
        std::fs::rename(&temp, self.folder.join(CACHE_FILE))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aemeath_protocol::ids::{NodeId, TopologyRevision};
    use aemeath_protocol::topology::{NodeKind, Resampler, Strip, TopologyNode};
    use aemeath_protocol::units::SampleRate;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aemeath-host-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn topology(revision: u64) -> RouteTopology {
        RouteTopology {
            revision: TopologyRevision(revision),
            engine_sample_rate: SampleRate::HZ_48000,
            clock_master: None,
            resampler: Resampler::default(),
            nodes: vec![TopologyNode {
                id: NodeId(1),
                kind: NodeKind::Bus,
                strip: Strip::default(),
                slot_settings: None,
                io: None,
            }],
            routes: vec![],
        }
    }

    #[test]
    fn round_trips_and_replaces_the_previous_topology() {
        let dir = temp_dir("cache");
        let cache = TopologyCache::new(&dir);
        assert_eq!(cache.load(), None);
        cache.save(&topology(1)).unwrap();
        cache.save(&topology(2)).unwrap();
        assert_eq!(cache.load(), Some(topology(2)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ignores_corrupt_or_invalid_files() {
        let dir = temp_dir("corrupt");
        let cache = TopologyCache::new(&dir);
        std::fs::create_dir_all(dir.join(CACHE_FOLDER)).unwrap();
        std::fs::write(dir.join(CACHE_FOLDER).join(CACHE_FILE), b"{not json").unwrap();
        assert_eq!(cache.load(), None);

        let mut invalid = topology(3);
        invalid.nodes.push(invalid.nodes[0].clone());
        std::fs::write(
            dir.join(CACHE_FOLDER).join(CACHE_FILE),
            serde_json::to_vec(&invalid).unwrap(),
        )
        .unwrap();
        assert_eq!(cache.load(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
