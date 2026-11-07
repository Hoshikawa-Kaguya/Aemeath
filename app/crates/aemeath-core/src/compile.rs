//! 从工作台编译路由拓扑（整体下发给引擎，ADR-0003）。
//!
//! 只保留引擎需要的内容：节点名、位置、连线标签位置是界面数据，不进入路由拓扑；
//! 外部设备只取端点 ID，设备名留在控制面做后备匹配。

use aemeath_protocol::ids::TopologyRevision;
use aemeath_protocol::topology::{RouteTopology, TopologyNode, TopologyRoute};

use crate::workspace::Workspace;

impl Workspace {
    /// 编译成路由拓扑。修订号由控制面维护（每次下发递增）。
    pub fn compile(&self, revision: TopologyRevision) -> RouteTopology {
        RouteTopology {
            revision,
            engine_sample_rate: self.engine.sample_rate,
            clock_master: self.engine.clock_master,
            resampler: self.engine.resampler,
            nodes: self
                .nodes()
                .iter()
                .map(|node| TopologyNode {
                    id: node.id,
                    kind: node.kind.map_device(|device| device.endpoint.clone()),
                    strip: node.strip,
                })
                .collect(),
            routes: self
                .routes()
                .iter()
                .map(|route| TopologyRoute {
                    from: route.from,
                    to: route.to,
                    gain: route.gain,
                    enabled: route.enabled,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DeviceRef, EngineSettings, NodeName, Position};
    use crate::workspace::NodeSpec;
    use aemeath_protocol::ids::EndpointId;
    use aemeath_protocol::units::GainDb;

    #[test]
    fn compiles_a_valid_topology() {
        let mut workspace = Workspace::new(EngineSettings::default());
        let name = |text: &str| NodeName::new(text).unwrap();
        let mic = workspace
            .add_node(
                name("Mic"),
                NodeSpec::ExternalInput(DeviceRef {
                    endpoint: EndpointId("{mic}".to_owned()),
                    name: "Rear Mic".to_owned(),
                }),
                Position::default(),
            )
            .unwrap();
        let obs = workspace
            .add_node(name("OBS"), NodeSpec::VirtualOutput(None), Position::default())
            .unwrap();
        workspace.connect(mic, obs).unwrap();
        workspace.set_route_gain(mic, obs, GainDb::new(-3.0).unwrap()).unwrap();
        workspace.set_clock_master(Some(mic)).unwrap();

        let topology = workspace.compile(TopologyRevision(4));
        assert_eq!(topology.validate(), Ok(()));
        assert_eq!(topology.revision, TopologyRevision(4));
        assert_eq!(topology.clock_master, Some(mic));
        assert_eq!(topology.nodes.len(), 2);
        assert_eq!(topology.routes[0].gain.db(), -3.0);
        assert!(matches!(
            &topology.nodes[0].kind,
            aemeath_protocol::topology::NodeKind::ExternalInput { device } if device.0 == "{mic}"
        ));
    }
}
