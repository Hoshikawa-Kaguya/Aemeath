//! 有向图的两个小算法：可达性与拓扑排序。
//!
//! 主程序（拖线时的成环检查）、拓扑校验和引擎（混音的处理顺序）共用，节点可以是 ID 也可以是下标。
//! 会分配内存，只在控制线程中使用。

use std::collections::HashMap;
use std::hash::Hash;

/// 沿边的方向从 `start` 能否走到 `goal`（`start == goal` 时为 true）。
pub fn reaches<N: Copy + Eq>(edges: &[(N, N)], start: N, goal: N) -> bool {
    let mut stack = vec![start];
    let mut seen = vec![start];
    while let Some(node) = stack.pop() {
        if node == goal {
            return true;
        }
        for &(_, next) in edges.iter().filter(|(from, _)| *from == node) {
            if !seen.contains(&next) {
                seen.push(next);
                stack.push(next);
            }
        }
    }
    false
}

/// Kahn 算法：返回一个拓扑顺序（边的起点先于终点）。
/// 有环时，环上和环下游的节点不在结果中；调用方可以据此判断是否有环。
pub fn topological_order<N: Copy + Eq + Hash>(nodes: &[N], edges: &[(N, N)]) -> Vec<N> {
    let mut incoming: HashMap<N, usize> = nodes.iter().map(|node| (*node, 0)).collect();
    let mut outgoing: HashMap<N, Vec<N>> = HashMap::new();
    for &(from, to) in edges {
        *incoming.entry(to).or_default() += 1;
        outgoing.entry(from).or_default().push(to);
    }
    let mut ready: Vec<N> = nodes
        .iter()
        .copied()
        .filter(|node| incoming.get(node) == Some(&0))
        .collect();
    let mut order = Vec::with_capacity(nodes.len());
    while let Some(node) = ready.pop() {
        order.push(node);
        for next in outgoing.get(&node).into_iter().flatten() {
            if let Some(count) = incoming.get_mut(next) {
                *count -= 1;
                if *count == 0 {
                    ready.push(*next);
                }
            }
        }
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_paths_along_the_edges_only() {
        let edges = [(1, 2), (2, 3), (4, 3)];
        assert!(reaches(&edges, 1, 3));
        assert!(reaches(&edges, 2, 2));
        assert!(!reaches(&edges, 3, 1));
        assert!(!reaches(&edges, 1, 4));
    }

    #[test]
    fn orders_sources_before_destinations() {
        let order = topological_order(&[3, 1, 2], &[(1, 2), (2, 3)]);
        assert_eq!(order, vec![1, 2, 3]);
    }

    #[test]
    fn leaves_nodes_on_a_cycle_out_of_the_order() {
        let order = topological_order(&[1, 2, 3, 4], &[(1, 2), (2, 3), (3, 2), (3, 4)]);
        assert_eq!(order, vec![1]);
    }
}
