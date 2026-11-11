//! 重复路由（GLOSSARY：重复路由）：同一个程序的声音同时经它的落点和它的应用源到达同一个目的地。
//!
//! 只警告不阻止。只看开启的连线，经过总线、外部插入间接到达的也算。

use std::collections::BTreeSet;

use aemeath_protocol::ids::NodeId;
use aemeath_protocol::topology::NodeRole;

use crate::workspace::Workspace;

/// 一处重复路由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateRoute {
    pub source: NodeId,
    pub destination: NodeId,
    /// 这次重复经过的全部开启的连线
    pub routes: BTreeSet<(NodeId, NodeId)>,
}

/// 从 `start` 沿连线能到达的全部节点（含它自己）。
fn reachable(edges: &[(NodeId, NodeId)], start: NodeId) -> BTreeSet<NodeId> {
    let mut seen = BTreeSet::from([start]);
    let mut stack = vec![start];
    while let Some(node) = stack.pop() {
        for &(_, next) in edges.iter().filter(|(from, _)| *from == node) {
            if seen.insert(next) {
                stack.push(next);
            }
        }
    }
    seen
}

/// 从 `start` 到 `destination` 的路径上的连线。
fn path_routes(
    edges: &[(NodeId, NodeId)],
    start: NodeId,
    destination: NodeId,
    routes: &mut BTreeSet<(NodeId, NodeId)>,
) {
    let from_start = reachable(edges, start);
    routes.extend(
        edges
            .iter()
            .filter(|(from, to)| from_start.contains(from) && reachable(edges, *to).contains(&destination)),
    );
}

/// `landings` 是（应用源节点，它的程序当前落在的节点）。落点不在工作台上的程序不要传进来。
pub fn duplicate_routes(workspace: &Workspace, landings: &[(NodeId, NodeId)]) -> Vec<DuplicateRoute> {
    let edges: Vec<(NodeId, NodeId)> = workspace
        .routes()
        .iter()
        .filter(|route| route.enabled)
        .map(|route| (route.from, route.to))
        .collect();
    let mut found: Vec<DuplicateRoute> = Vec::new();
    for &(source, landing) in landings {
        let Some(landing_node) = workspace.node(landing) else {
            continue;
        };
        let from_source = reachable(&edges, source);
        // 落点是外部输出：声音直接到了这个节点，只看应用源能否到达它；落点是虚拟输入：比较两边能到达的节点
        let destinations: BTreeSet<NodeId> = if landing_node.kind.role() == NodeRole::Output {
            from_source.intersection(&BTreeSet::from([landing])).copied().collect()
        } else {
            let mut common: BTreeSet<NodeId> = from_source.intersection(&reachable(&edges, landing)).copied().collect();
            common.remove(&source);
            common.remove(&landing);
            common
        };
        // 只报告终点：沿途的总线也在交集里，但声音"出现两份"的地方是输出节点
        for destination in destinations.iter().copied().filter(|node| {
            workspace
                .node(*node)
                .is_some_and(|node| node.kind.role() == NodeRole::Output)
        }) {
            let mut routes = BTreeSet::new();
            path_routes(&edges, source, destination, &mut routes);
            path_routes(&edges, landing, destination, &mut routes);
            // 一个程序常有多个音频会话，`landings` 里会有多条：同一处重复只报告一次，连线合并
            match found
                .iter_mut()
                .find(|own| own.source == source && own.destination == destination)
            {
                Some(existing) => existing.routes.extend(routes),
                None => found.push(DuplicateRoute {
                    source,
                    destination,
                    routes,
                }),
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DeviceRef, EngineSettings, NodeName, Position};
    use crate::workspace::NodeSpec;
    use aemeath_protocol::ids::{EndpointId, ProgramPath};

    fn add(workspace: &mut Workspace, spec: NodeSpec) -> NodeId {
        workspace
            .add_node(NodeName::new("n").unwrap(), spec, Position::default())
            .unwrap()
    }

    fn output(id: &str) -> NodeSpec {
        NodeSpec::ExternalOutput(DeviceRef {
            endpoint: EndpointId(id.to_owned()),
            name: id.to_owned(),
        })
    }

    struct Fixture {
        workspace: Workspace,
        source: NodeId,
        landing: NodeId,
        bus: NodeId,
        destination: NodeId,
    }

    fn fixture() -> Fixture {
        let mut workspace = Workspace::new(EngineSettings::default());
        let source = add(
            &mut workspace,
            NodeSpec::AppSource(ProgramPath::new(r"C:\a.exe").unwrap()),
        );
        let landing = add(&mut workspace, NodeSpec::VirtualInput(None));
        let bus = add(&mut workspace, NodeSpec::Bus);
        let destination = add(&mut workspace, NodeSpec::VirtualOutput(None));
        Fixture {
            workspace,
            source,
            landing,
            bus,
            destination,
        }
    }

    #[test]
    fn warns_when_both_paths_reach_the_same_destination_through_a_bus() {
        let Fixture {
            mut workspace,
            source,
            landing,
            bus,
            destination,
        } = fixture();
        workspace.connect(source, bus).unwrap();
        workspace.connect(landing, bus).unwrap();
        workspace.connect(bus, destination).unwrap();
        let found = duplicate_routes(&workspace, &[(source, landing)]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].destination, destination);
        assert_eq!(
            found[0].routes,
            BTreeSet::from([(source, bus), (landing, bus), (bus, destination)])
        );
    }

    #[test]
    fn several_sessions_of_one_program_report_one_duplicate_with_all_routes() {
        let Fixture {
            mut workspace,
            source,
            landing,
            destination,
            ..
        } = fixture();
        let second_landing = add(&mut workspace, NodeSpec::VirtualInput(None));
        workspace.connect(source, destination).unwrap();
        workspace.connect(landing, destination).unwrap();
        workspace.connect(second_landing, destination).unwrap();
        let found = duplicate_routes(
            &workspace,
            &[(source, landing), (source, landing), (source, second_landing)],
        );
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].routes,
            BTreeSet::from([
                (source, destination),
                (landing, destination),
                (second_landing, destination)
            ])
        );
    }

    #[test]
    fn disabled_routes_and_separate_destinations_do_not_count() {
        let Fixture {
            mut workspace,
            source,
            landing,
            destination,
            ..
        } = fixture();
        let other = add(&mut workspace, NodeSpec::VirtualOutput(None));
        workspace.connect(source, destination).unwrap();
        workspace.connect(landing, other).unwrap();
        assert!(duplicate_routes(&workspace, &[(source, landing)]).is_empty());

        workspace.connect(landing, destination).unwrap();
        assert_eq!(duplicate_routes(&workspace, &[(source, landing)]).len(), 1);
        workspace.set_route_enabled(landing, destination, false).unwrap();
        assert!(duplicate_routes(&workspace, &[(source, landing)]).is_empty());
    }

    #[test]
    fn a_landing_on_an_external_output_warns_when_the_source_reaches_it() {
        let Fixture {
            mut workspace,
            source,
            bus,
            ..
        } = fixture();
        let speakers = add(&mut workspace, output("speakers"));
        assert!(duplicate_routes(&workspace, &[(source, speakers)]).is_empty());
        workspace.connect(source, bus).unwrap();
        workspace.connect(bus, speakers).unwrap();
        let found = duplicate_routes(&workspace, &[(source, speakers)]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].destination, speakers);
    }
}
