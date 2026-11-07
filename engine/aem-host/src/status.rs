//! 槽位与外部设备状态的缓存，以及引擎事件到 IPC 消息的转换。
//!
//! 主程序随时可能（重新）连接：连接后先收到一份状态快照，之后收到变化。没有客户端时事件只更新缓存。
//! 外部设备的状态按"端点 + 方向"记录，发送时按当前计划对应到节点（协议里的状态以节点为单位）。

use std::collections::{BTreeMap, HashMap};

use aem_engine::EngineEvent;
use aem_io::{IoDirection, IoEvent};
use aem_protocol::ids::{EndpointId, NodeId, SlotId};
use aem_protocol::message::{DeviceFormat, EndpointStatus, EngineToApp};
use aem_protocol::plan::{NodeKind, RoutePlan};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct SlotState {
    connected: bool,
    format: Option<DeviceFormat>,
}

#[derive(Debug, Default)]
pub struct StatusCache {
    slots: BTreeMap<SlotId, SlotState>,
    endpoints: HashMap<(EndpointId, IoDirection), EndpointStatus>,
}

impl StatusCache {
    pub fn record(&mut self, event: &EngineEvent) {
        match event {
            EngineEvent::SlotConnection { slot, connected } => {
                let state = self.slots.entry(*slot).or_default();
                state.connected = *connected;
                if !connected {
                    state.format = None;
                }
            }
            EngineEvent::SlotFormat { slot, format } => self.slots.entry(*slot).or_default().format = *format,
            EngineEvent::Endpoint(event) => {
                self.endpoints
                    .insert((event.endpoint.clone(), event.direction), event.status);
            }
        }
    }

    /// 当前全部状态，按 `plan` 把外部设备对应到节点。
    pub fn snapshot(&self, plan: Option<&RoutePlan>) -> Vec<EngineToApp> {
        let mut messages = Vec::new();
        for (slot, state) in &self.slots {
            messages.push(EngineToApp::SlotConnection {
                slot: *slot,
                connected: state.connected,
            });
            if state.format.is_some() {
                messages.push(EngineToApp::SlotFormat {
                    slot: *slot,
                    format: state.format,
                });
            }
        }
        for ((endpoint, direction), status) in &self.endpoints {
            for node in nodes_for(plan, endpoint, *direction) {
                messages.push(EngineToApp::EndpointStatus { node, status: *status });
            }
        }
        messages
    }
}

/// 一个引擎事件对应的 IPC 消息（外部设备事件按当前计划对应到节点；计划里没有这台设备时没有消息）。
pub fn to_messages(event: &EngineEvent, plan: Option<&RoutePlan>) -> Vec<EngineToApp> {
    match event {
        EngineEvent::SlotConnection { slot, connected } => vec![EngineToApp::SlotConnection {
            slot: *slot,
            connected: *connected,
        }],
        EngineEvent::SlotFormat { slot, format } => vec![EngineToApp::SlotFormat {
            slot: *slot,
            format: *format,
        }],
        EngineEvent::Endpoint(IoEvent {
            endpoint,
            direction,
            status,
        }) => nodes_for(plan, endpoint, *direction)
            .map(|node| EngineToApp::EndpointStatus { node, status: *status })
            .collect(),
    }
}

fn nodes_for<'a>(
    plan: Option<&'a RoutePlan>,
    endpoint: &'a EndpointId,
    direction: IoDirection,
) -> impl Iterator<Item = NodeId> + 'a {
    plan.into_iter()
        .flat_map(|plan| &plan.nodes)
        .filter(move |node| match (&node.kind, direction) {
            (NodeKind::ExternalInput { endpoint: own }, IoDirection::Input)
            | (NodeKind::ExternalOutput { endpoint: own }, IoDirection::Output) => own == endpoint,
            _ => false,
        })
        .map(|node| node.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aem_protocol::ids::{PlanRevision, SlotDirection, SlotNumber};
    use aem_protocol::message::OfflineReason;
    use aem_protocol::plan::{PlanNode, Resampler, Strip};
    use aem_protocol::units::SampleRate;

    const MIC: NodeId = NodeId(5);

    fn microphone() -> EndpointId {
        EndpointId("{0.0.1.00000000}.{mic}".to_owned())
    }

    fn plan() -> RoutePlan {
        RoutePlan {
            revision: PlanRevision(1),
            engine_sample_rate: SampleRate::HZ_48000,
            clock_master: None,
            resampler: Resampler::default(),
            nodes: vec![PlanNode {
                id: MIC,
                kind: NodeKind::ExternalInput { endpoint: microphone() },
                strip: Strip::default(),
            }],
            routes: vec![],
        }
    }

    fn offline(direction: IoDirection) -> EngineEvent {
        EngineEvent::Endpoint(IoEvent {
            endpoint: microphone(),
            direction,
            status: EndpointStatus::Offline {
                reason: OfflineReason::WaitingForDevice,
            },
        })
    }

    #[test]
    fn device_events_map_to_nodes_of_the_matching_direction() {
        let plan = plan();
        assert_eq!(
            to_messages(&offline(IoDirection::Input), Some(&plan)),
            vec![EngineToApp::EndpointStatus {
                node: MIC,
                status: EndpointStatus::Offline {
                    reason: OfflineReason::WaitingForDevice,
                },
            }]
        );
        assert!(to_messages(&offline(IoDirection::Output), Some(&plan)).is_empty());
        assert!(to_messages(&offline(IoDirection::Input), None).is_empty());
    }

    #[test]
    fn snapshot_reports_the_latest_state() {
        let slot = SlotId {
            direction: SlotDirection::Out,
            number: SlotNumber::new(3).unwrap(),
        };
        let mut cache = StatusCache::default();
        cache.record(&EngineEvent::SlotConnection { slot, connected: true });
        cache.record(&offline(IoDirection::Input));
        cache.record(&EngineEvent::SlotConnection { slot, connected: false });
        let snapshot = cache.snapshot(Some(&plan()));
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[0], EngineToApp::SlotConnection { slot, connected: false });
    }
}
