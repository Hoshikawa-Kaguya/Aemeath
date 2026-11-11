//! 节点电平：调度线程写入峰值，主程序一侧定期取走。
//!
//! 每份路由拓扑配一组电平；读写都是原子操作，不加锁、不分配内存。
//! 峰值是非负浮点数，按位比较与按值比较的顺序一致，所以可以用整数的 `fetch_max` 记录。

use std::sync::atomic::{AtomicU32, Ordering};

use aemeath_protocol::ids::NodeId;

use crate::STEREO;

/// 各节点左右声道的峰值。调度线程用原子量写入，不加锁；读取方取走后清零。
#[derive(Debug)]
pub struct MeterBank {
    node_ids: Vec<NodeId>,
    peaks: Vec<[AtomicU32; STEREO]>,
}

impl MeterBank {
    /// 节点顺序与路由拓扑中的 `nodes` 一致。
    pub fn new(node_ids: Vec<NodeId>) -> Self {
        let peaks = node_ids
            .iter()
            .map(|_| [AtomicU32::new(0), AtomicU32::new(0)])
            .collect();
        Self { node_ids, peaks }
    }

    pub fn node_ids(&self) -> &[NodeId] {
        &self.node_ids
    }

    /// 调度线程调用：记录一个块的峰值（取已记录值与新值中的较大者）。
    pub fn record(&self, index: usize, peaks: [f32; STEREO]) {
        if let Some(slots) = self.peaks.get(index) {
            for (slot, peak) in slots.iter().zip(peaks) {
                slot.fetch_max(peak.abs().to_bits(), Ordering::Relaxed);
            }
        }
    }

    /// 读取方调用：取走自上次读取以来的峰值，并清零。
    pub fn take(&self, index: usize) -> Option<[f32; STEREO]> {
        let slots = self.peaks.get(index)?;
        Some([0, 1].map(|channel| f32::from_bits(slots[channel].swap(0, Ordering::Relaxed))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_largest_peak_until_taken() {
        let meters = MeterBank::new(vec![NodeId(7)]);
        meters.record(0, [0.25, -0.5]);
        meters.record(0, [0.75, 0.1]);
        assert_eq!(meters.take(0), Some([0.75, 0.5]));
        assert_eq!(meters.take(0), Some([0.0, 0.0]));
        assert_eq!(meters.take(1), None);
    }
}
