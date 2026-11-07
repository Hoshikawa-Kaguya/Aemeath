//! 判断一个槽位的挂载是否正常（ADR-0007），纯函数，不做 IO。
//!
//! 被占用的槽位，三者同时满足才算正常：
//! 1. usbip-win2 的已导入列表里，有且只有一个指向 Aemeath 服务端口和该 busid 的条目；
//! 2. USB/IP 服务端报告该设备的连接仍然有效；
//! 3. 对应的音频端点处于 ACTIVE 状态。刚挂上时 Windows 需要一点时间（首次安装驱动可能接近 1 分钟），
//!    所以连接建立后的一段宽限期内只算"等待端点出现"。
//!
//! 没有被占用的槽位保持卸下（ADR-0009"拔出"）：列表里还有它的条目就要卸下。

use std::time::Duration;

use aemeath_sys::audio_endpoints::EndpointState;

use crate::usbip_cli::VhciPort;

/// 一个槽位此刻的观测结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observation<'a> {
    pub occupied: bool,
    /// 已导入列表里指向 Aemeath 服务端口、busid 为这个槽位的条目
    pub ports: &'a [VhciPort],
    pub server_connected: bool,
    /// 端点状态；找不到端点时为 None
    pub endpoint: Option<EndpointState>,
    /// 服务端连接已经保持了多久；没有连接时为 None
    pub connected_for: Option<Duration>,
}

/// 需要修复的原因（写进日志，也用于界面提示）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairReason {
    NotImported,
    /// 列表里有多个条目（残留的后台重连等）
    DuplicateImport,
    /// 列表里有条目，但服务端没有这条连接（残留条目）
    ServerNotConnected,
    /// 连接正常，但宽限期过后端点仍不是 ACTIVE
    EndpointInactive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Assessment {
    Healthy,
    /// 连接已建立，等待端点出现
    Settling,
    Repair(RepairReason),
    /// 没有被占用，但列表里还有条目：需要卸下
    Unplug,
    /// 没有被占用，已经卸下
    Unplugged,
}

pub fn assess(observation: &Observation<'_>, endpoint_grace: Duration) -> Assessment {
    if !observation.occupied {
        return if observation.ports.is_empty() {
            Assessment::Unplugged
        } else {
            Assessment::Unplug
        };
    }
    match observation.ports.len() {
        0 => return Assessment::Repair(RepairReason::NotImported),
        1 => {}
        _ => return Assessment::Repair(RepairReason::DuplicateImport),
    }
    if !observation.server_connected {
        return Assessment::Repair(RepairReason::ServerNotConnected);
    }
    match observation.endpoint {
        Some(EndpointState::Active) => Assessment::Healthy,
        _ if observation
            .connected_for
            .is_some_and(|elapsed| elapsed < endpoint_grace) =>
        {
            Assessment::Settling
        }
        _ => Assessment::Repair(RepairReason::EndpointInactive),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRACE: Duration = Duration::from_secs(10);
    const ONE: [VhciPort; 1] = [VhciPort(1)];
    const TWO: [VhciPort; 2] = [VhciPort(1), VhciPort(5)];

    fn healthy() -> Observation<'static> {
        Observation {
            occupied: true,
            ports: &ONE,
            server_connected: true,
            endpoint: Some(EndpointState::Active),
            connected_for: Some(Duration::from_secs(60)),
        }
    }

    #[test]
    fn all_three_conditions_make_a_slot_healthy() {
        assert_eq!(assess(&healthy(), GRACE), Assessment::Healthy);
    }

    #[test]
    fn missing_or_duplicate_imports_need_repair() {
        let missing = Observation {
            ports: &[],
            ..healthy()
        };
        assert_eq!(assess(&missing, GRACE), Assessment::Repair(RepairReason::NotImported));
        let duplicate = Observation {
            ports: &TWO,
            ..healthy()
        };
        assert_eq!(
            assess(&duplicate, GRACE),
            Assessment::Repair(RepairReason::DuplicateImport)
        );
    }

    #[test]
    fn a_stale_entry_without_a_server_connection_needs_repair() {
        let stale = Observation {
            server_connected: false,
            connected_for: None,
            ..healthy()
        };
        assert_eq!(
            assess(&stale, GRACE),
            Assessment::Repair(RepairReason::ServerNotConnected)
        );
    }

    #[test]
    fn the_endpoint_gets_a_grace_period_after_connecting() {
        let fresh = Observation {
            endpoint: Some(EndpointState::NotPresent),
            connected_for: Some(Duration::from_secs(2)),
            ..healthy()
        };
        assert_eq!(assess(&fresh, GRACE), Assessment::Settling);
        let missing_endpoint = Observation {
            endpoint: None,
            ..fresh
        };
        assert_eq!(assess(&missing_endpoint, GRACE), Assessment::Settling);
        let late = Observation {
            connected_for: Some(GRACE),
            ..fresh
        };
        assert_eq!(assess(&late, GRACE), Assessment::Repair(RepairReason::EndpointInactive));
    }

    #[test]
    fn unoccupied_slots_are_kept_unplugged() {
        let leftover = Observation {
            occupied: false,
            ..healthy()
        };
        assert_eq!(assess(&leftover, GRACE), Assessment::Unplug);
        let clean = Observation {
            occupied: false,
            ports: &[],
            server_connected: false,
            endpoint: Some(EndpointState::NotPresent),
            connected_for: None,
        };
        assert_eq!(assess(&clean, GRACE), Assessment::Unplugged);
    }
}
