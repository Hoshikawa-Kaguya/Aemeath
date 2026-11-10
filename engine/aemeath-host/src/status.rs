//! 槽位与外部设备状态的缓存，以及引擎事件到 IPC 消息的转换。
//!
//! 主程序随时可能（重新）连接：连接后先收到一份状态快照，之后收到变化。没有客户端时事件只更新缓存。
//! 外部设备的状态按"端点 + 方向"记录，发送时按当前拓扑对应到节点（协议里的状态以节点为单位）。

use std::collections::{BTreeMap, HashMap};

use aemeath_engine::EngineEvent;
use aemeath_io::app_source::AppSourceEvent;
use aemeath_io::{IoDirection, IoEvent};
use aemeath_protocol::ids::{EndpointId, NodeId, ProgramPath, SlotId};
use aemeath_protocol::message::{DeviceFormat, EndpointStatus, EngineToApp, RecorderState};
use aemeath_protocol::topology::{NodeRole, RouteTopology};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct SlotState {
    connected: bool,
    format: Option<DeviceFormat>,
}

#[derive(Debug, Default)]
pub struct StatusCache {
    slots: BTreeMap<SlotId, SlotState>,
    endpoints: HashMap<(EndpointId, IoDirection), EndpointStatus>,
    programs: HashMap<ProgramPath, EndpointStatus>,
    recorders: BTreeMap<NodeId, RecorderState>,
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
            EngineEvent::AppSource(event) => {
                self.programs.insert(event.program.clone(), event.status);
            }
            EngineEvent::Recorder { node, state } => {
                self.recorders.insert(*node, state.clone());
            }
        }
    }

    /// 当前全部状态，按 `topology` 把外部设备对应到节点。
    pub fn snapshot(&self, topology: Option<&RouteTopology>) -> Vec<EngineToApp> {
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
            for node in nodes_for(topology, endpoint, *direction) {
                messages.push(EngineToApp::EndpointStatus { node, status: *status });
            }
        }
        for (program, status) in &self.programs {
            messages.extend(
                node_for_program(topology, program).map(|node| EngineToApp::EndpointStatus { node, status: *status }),
            );
        }
        for (node, state) in &self.recorders {
            messages.push(EngineToApp::RecorderStatus {
                node: *node,
                status: state.clone(),
            });
        }
        messages
    }
}

/// 一个引擎事件对应的 IPC 消息（外部设备事件按当前拓扑对应到节点；拓扑里没有这台设备时没有消息）。
pub fn to_messages(event: &EngineEvent, topology: Option<&RouteTopology>) -> Vec<EngineToApp> {
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
        }) => nodes_for(topology, endpoint, *direction)
            .map(|node| EngineToApp::EndpointStatus { node, status: *status })
            .collect(),
        EngineEvent::Recorder { node, state } => vec![EngineToApp::RecorderStatus {
            node: *node,
            status: state.clone(),
        }],
        EngineEvent::AppSource(AppSourceEvent { program, status }) => node_for_program(topology, program)
            .map(|node| EngineToApp::EndpointStatus { node, status: *status })
            .into_iter()
            .collect(),
    }
}

fn node_for_program(topology: Option<&RouteTopology>, program: &ProgramPath) -> Option<NodeId> {
    topology?
        .nodes
        .iter()
        .find(|node| node.kind.program() == Some(program))
        .map(|node| node.id)
}

fn nodes_for<'a>(
    topology: Option<&'a RouteTopology>,
    endpoint: &'a EndpointId,
    direction: IoDirection,
) -> impl Iterator<Item = NodeId> + 'a {
    topology
        .into_iter()
        .flat_map(|topology| &topology.nodes)
        .filter(move |node| {
            let wanted = match direction {
                IoDirection::Input => NodeRole::Input,
                IoDirection::Output => NodeRole::Output,
            };
            node.kind.endpoint_role() == Some(wanted) && node.kind.device() == Some(endpoint)
        })
        .map(|node| node.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aemeath_protocol::ids::{SlotDirection, SlotNumber, TopologyRevision};
    use aemeath_protocol::message::OfflineReason;
    use aemeath_protocol::topology::{NodeKind, Resampler, Strip, TopologyNode};
    use aemeath_protocol::units::SampleRate;

    const MIC: NodeId = NodeId(5);

    fn microphone() -> EndpointId {
        EndpointId("{0.0.1.00000000}.{mic}".to_owned())
    }

    fn topology() -> RouteTopology {
        RouteTopology {
            revision: TopologyRevision(1),
            engine_sample_rate: SampleRate::HZ_48000,
            clock_master: None,
            resampler: Resampler::default(),
            nodes: vec![TopologyNode {
                id: MIC,
                kind: NodeKind::ExternalInput { device: microphone() },
                strip: Strip::default(),
                slot_settings: None,
                io: None,
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
        let topology = topology();
        assert_eq!(
            to_messages(&offline(IoDirection::Input), Some(&topology)),
            vec![EngineToApp::EndpointStatus {
                node: MIC,
                status: EndpointStatus::Offline {
                    reason: OfflineReason::WaitingForDevice,
                },
            }]
        );
        assert!(to_messages(&offline(IoDirection::Output), Some(&topology)).is_empty());
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
        let snapshot = cache.snapshot(Some(&topology()));
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[0], EngineToApp::SlotConnection { slot, connected: false });
    }
}
