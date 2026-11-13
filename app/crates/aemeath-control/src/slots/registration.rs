//! 首次启动时注册全部 16 个槽位：每个槽位挂载一次，让 Windows 完成驱动安装，
//! 并把设备格式设成引擎采样率（应在槽位还没被应用使用时设定）。
//!
//! 注册不是单独的机制，而是挂载维护的一种用法：
//! 1. 下发 [`registration_topology`]，让服务端导出全部槽位（服务端只导出拓扑中被占用的槽位）；
//! 2. 把全部槽位设为被占用（[`super::SlotMaintainer::set_occupied`]）；
//! 3. 用 [`RegistrationProgress`] 跟踪，直到全部完成；
//! 4. 换回正式的拓扑和占用集合，挂载维护随即卸下没有被占用的槽位。
//!
//! 第一次安装驱动时每个槽位可能接近 1 分钟，所以界面要显示进度。是否已经注册不记在配置里，
//! 主程序每次启动时查设备实例判断（[`super::uninstall::all_slots_registered`]）。

use std::collections::{BTreeMap, BTreeSet};

use aemeath_protocol::ids::{NodeId, SlotDirection, SlotId, TopologyRevision};
use aemeath_protocol::topology::{NodeKind, Resampler, RouteTopology, SlotFormatChoice, Strip, TopologyNode};
use aemeath_protocol::units::SampleRate;

use super::maintainer::{SlotHealth, SlotStatus, desired_format};

/// 全部 16 个槽位。
pub fn all_slots() -> BTreeSet<SlotId> {
    [SlotDirection::In, SlotDirection::Out]
        .into_iter()
        .flat_map(SlotId::all)
        .collect()
}

/// 注册用的路由拓扑：每个槽位一个虚拟节点，没有连线（全部静音）。
pub fn registration_topology(rate: SampleRate) -> RouteTopology {
    let nodes = all_slots()
        .into_iter()
        .zip(1..)
        .map(|(slot, id)| TopologyNode {
            id: NodeId(id),
            kind: match slot.direction {
                SlotDirection::In => NodeKind::VirtualInput { slot },
                SlotDirection::Out => NodeKind::VirtualOutput { slot },
            },
            strip: Strip::default(),
            slot_settings: None,
            io: None,
        })
        .collect();
    RouteTopology {
        revision: TopologyRevision(0),
        engine_sample_rate: rate,
        clock_master: None,
        resampler: Resampler::default(),
        nodes,
        routes: Vec::new(),
    }
}

/// 注册进度：一个槽位挂载正常、并且设备格式已经是引擎采样率，才算完成。
#[derive(Debug, Clone)]
pub struct RegistrationProgress {
    rate: SampleRate,
    latest: BTreeMap<SlotId, SlotStatus>,
}

impl RegistrationProgress {
    pub fn new(rate: SampleRate) -> Self {
        Self {
            // 槽位的格式由挂载维护决定，引擎采样率超过槽位上限时封顶
            rate: desired_format(SlotFormatChoice::FollowEngine, rate).0,
            latest: BTreeMap::new(),
        }
    }

    /// 记录挂载维护报告的最新状态。
    pub fn update(&mut self, status: SlotStatus) {
        self.latest.insert(status.slot, status);
    }

    /// 这个槽位是否已经完成注册（判据见 [`RegistrationProgress`]）。
    pub fn is_registered(&self, slot: SlotId) -> bool {
        self.latest.get(&slot).is_some_and(|status| {
            status.health == SlotHealth::Healthy
                && status
                    .endpoint
                    .as_ref()
                    .and_then(|endpoint| endpoint.format)
                    .is_some_and(|format| format.sample_rate == self.rate)
        })
    }

    /// 已完成的槽位数
    pub fn completed(&self) -> usize {
        all_slots().into_iter().filter(|slot| self.is_registered(*slot)).count()
    }

    pub fn total(&self) -> usize {
        all_slots().len()
    }

    /// 全部 16 个槽位都已完成。
    pub fn is_done(&self) -> bool {
        self.completed() == self.total()
    }

    /// 多次修复失败、需要提示用户的槽位
    pub fn failing(&self) -> Vec<SlotId> {
        self.latest
            .values()
            .filter(|status| matches!(status.health, SlotHealth::Failing { .. }))
            .map(|status| status.slot)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slots::endpoints::SlotEndpoint;
    use aemeath_protocol::message::{DeviceFormat, SampleFormat};
    use aemeath_sys::audio_endpoints::EndpointState;

    #[test]
    fn the_registration_topology_occupies_every_slot_and_is_valid() {
        let topology = registration_topology(SampleRate::HZ_48000);
        assert_eq!(topology.validate(), Ok(()));
        let occupied: BTreeSet<SlotId> = topology.nodes.iter().filter_map(|node| node.kind.slot()).collect();
        assert_eq!(occupied, all_slots());
        assert_eq!(occupied.len(), 16);
    }

    #[test]
    fn a_slot_counts_once_it_is_healthy_at_the_engine_rate() {
        let rate = SampleRate::HZ_48000;
        let mut progress = RegistrationProgress::new(rate);
        let slot = *all_slots().first().unwrap();
        let status = |health, sample_rate| SlotStatus {
            slot,
            health,
            endpoint: Some(SlotEndpoint {
                id: "{id}".to_owned(),
                name: None,
                state: EndpointState::Active,
                format: Some(DeviceFormat {
                    sample_rate,
                    sample_format: SampleFormat::Int32,
                    channels: 2,
                }),
            }),
        };
        progress.update(status(SlotHealth::Settling, rate));
        assert_eq!(progress.completed(), 0);
        progress.update(status(SlotHealth::Healthy, SampleRate::new(44_100).unwrap()));
        assert_eq!(progress.completed(), 0, "the sample rate has not been set yet");
        progress.update(status(SlotHealth::Healthy, rate));
        assert_eq!(progress.completed(), 1);
        assert!(!progress.is_done());
    }

    #[test]
    fn above_the_slot_maximum_a_slot_counts_at_the_maximum() {
        let mut progress = RegistrationProgress::new(SampleRate::new(768_000).unwrap());
        let slot = *all_slots().first().unwrap();
        progress.update(SlotStatus {
            slot,
            health: SlotHealth::Healthy,
            endpoint: Some(SlotEndpoint {
                id: "{id}".to_owned(),
                name: None,
                state: EndpointState::Active,
                format: Some(DeviceFormat {
                    sample_rate: SampleRate::new(384_000).unwrap(),
                    sample_format: SampleFormat::Int32,
                    channels: 2,
                }),
            }),
        });
        assert_eq!(progress.completed(), 1);
    }
}
