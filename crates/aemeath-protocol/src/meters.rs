//! 电平共享内存的布局（电平不走管道，走共享内存）。引擎写，主程序读。
//!
//! 整块内存是 [`METER_MEMORY_WORDS`] 个 `AtomicU32`：
//!
//! | 字 | 内容 |
//! |---|---|
//! | 0 | [`METER_MAGIC`] |
//! | 1 | [`METER_LAYOUT_VERSION`] |
//! | 2 | 节点表的代数：写节点表时为奇数，写完为偶数（seqlock） |
//! | 3 | 节点数 |
//! | 4 起 | 每个节点 3 个字：节点 ID、左声道峰值、右声道峰值（`f32` 的位） |
//!
//! - 峰值是非负浮点数，按位比较与按值比较的顺序一致，写入方用 `fetch_max` 累加，读取方用 `swap(0)` 取走：
//!   读得再慢也不会漏掉峰值。
//! - 换拓扑时写入方按 seqlock 重写节点表；读取方发现代数变了，就丢弃这一次读到的数据。

use std::sync::atomic::{AtomicU32, Ordering};

use thiserror::Error;

use crate::ids::NodeId;
use crate::topology::MAX_NODES;

/// "AEMM"
pub const METER_MAGIC: u32 = u32::from_le_bytes(*b"AEMM");
/// 布局有任何变化都要加一
pub const METER_LAYOUT_VERSION: u32 = 1;
/// 最多的节点数，与路由拓扑的上限相同
pub const METER_CAPACITY: usize = MAX_NODES;
/// 每个节点记录的声道数。这是共享内存布局的一部分（与引擎内部的 `aemeath_dsp::STEREO` 数值相同但含义不同），
/// 协议也不依赖 dsp，所以单独定义；写入方用编译期断言确认两者一致。
pub const METER_CHANNELS: usize = 2;

const MAGIC_WORD: usize = 0;
const VERSION_WORD: usize = 1;
const GENERATION_WORD: usize = 2;
const COUNT_WORD: usize = 3;
const HEADER_WORDS: usize = 4;
const ENTRY_NODE: usize = 0;
const ENTRY_PEAKS: usize = 1;
const ENTRY_WORDS: usize = 1 + METER_CHANNELS;
/// 整块共享内存的字数
pub const METER_MEMORY_WORDS: usize = HEADER_WORDS + METER_CAPACITY * ENTRY_WORDS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("the meter memory has {0} words, fewer than {METER_MEMORY_WORDS}")]
pub struct TooSmall(pub usize);

/// 一个节点的电平：自上次读取以来的峰值（线性幅度，1.0 为满幅）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NodeMeter {
    pub node: NodeId,
    pub peaks: [f32; METER_CHANNELS],
}

fn check(words: &[AtomicU32]) -> Result<&[AtomicU32], TooSmall> {
    if words.len() < METER_MEMORY_WORDS {
        Err(TooSmall(words.len()))
    } else {
        Ok(&words[..METER_MEMORY_WORDS])
    }
}

fn entry(index: usize) -> usize {
    HEADER_WORDS + index * ENTRY_WORDS
}

/// 写入方（引擎）。同一时间只能有一个写入者。
#[derive(Debug)]
pub struct MeterWriter<'a> {
    words: &'a [AtomicU32],
}

impl<'a> MeterWriter<'a> {
    /// 写入表头。内存可能是上一个写入者留下的（引擎重启），节点表在 [`Self::set_nodes`] 时重写。
    pub fn new(words: &'a [AtomicU32]) -> Result<Self, TooSmall> {
        let words = check(words)?;
        words[MAGIC_WORD].store(METER_MAGIC, Ordering::Release);
        words[VERSION_WORD].store(METER_LAYOUT_VERSION, Ordering::Release);
        // 上一个写入者在写节点表时退出，代数会停在奇数；回到偶数，节点表随后重写
        let generation = words[GENERATION_WORD].load(Ordering::Acquire);
        if generation % 2 == 1 {
            words[GENERATION_WORD].store(generation.wrapping_add(1), Ordering::Release);
        }
        Ok(Self { words })
    }

    /// 换成新的节点表（顺序与路由拓扑的 `nodes` 一致），峰值清零。超出容量的节点不显示电平。
    pub fn set_nodes(&self, nodes: &[NodeId]) {
        let count = nodes.len().min(METER_CAPACITY);
        let generation = self.words[GENERATION_WORD].load(Ordering::Acquire);
        self.words[GENERATION_WORD].store(generation.wrapping_add(1), Ordering::SeqCst);
        for (index, node) in nodes.iter().take(count).enumerate() {
            let base = entry(index);
            self.words[base + ENTRY_NODE].store(node.0, Ordering::Relaxed);
            for channel in 0..METER_CHANNELS {
                self.words[base + ENTRY_PEAKS + channel].store(0, Ordering::Relaxed);
            }
        }
        // count 不超过 METER_CAPACITY（256），放得进 u32
        self.words[COUNT_WORD].store(count as u32, Ordering::Relaxed);
        self.words[GENERATION_WORD].store(generation.wrapping_add(2), Ordering::SeqCst);
    }

    /// 累加一个节点的峰值（取已记录值与新值中的较大者）。非有限值按 0 处理。
    pub fn record(&self, index: usize, peaks: [f32; METER_CHANNELS]) {
        if index >= METER_CAPACITY {
            return;
        }
        let base = entry(index);
        for (channel, peak) in peaks.into_iter().enumerate() {
            let peak = if peak.is_finite() { peak.abs() } else { 0.0 };
            self.words[base + ENTRY_PEAKS + channel].fetch_max(peak.to_bits(), Ordering::Relaxed);
        }
    }
}

/// 读取方（主程序）。
#[derive(Debug)]
pub struct MeterReader<'a> {
    words: &'a [AtomicU32],
}

impl<'a> MeterReader<'a> {
    /// 在共享内存上建读取器；内存比约定的布局小时报错，不越界读取。
    pub fn new(words: &'a [AtomicU32]) -> Result<Self, TooSmall> {
        Ok(Self { words: check(words)? })
    }

    /// 取走全部节点自上次读取以来的峰值。写入方还没准备好、布局版本不同或者节点表正在变化时返回 None。
    pub fn take(&self) -> Option<Vec<NodeMeter>> {
        if self.words[MAGIC_WORD].load(Ordering::Acquire) != METER_MAGIC
            || self.words[VERSION_WORD].load(Ordering::Acquire) != METER_LAYOUT_VERSION
        {
            return None;
        }
        let before = self.words[GENERATION_WORD].load(Ordering::SeqCst);
        if before % 2 == 1 {
            return None;
        }
        let count = (self.words[COUNT_WORD].load(Ordering::Relaxed) as usize).min(METER_CAPACITY);
        let meters = (0..count)
            .map(|index| {
                let base = entry(index);
                NodeMeter {
                    node: NodeId(self.words[base + ENTRY_NODE].load(Ordering::Relaxed)),
                    peaks: std::array::from_fn(|channel| {
                        f32::from_bits(self.words[base + ENTRY_PEAKS + channel].swap(0, Ordering::Relaxed))
                    }),
                }
            })
            .collect();
        let after = self.words[GENERATION_WORD].load(Ordering::SeqCst);
        (before == after).then_some(meters)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory() -> Vec<AtomicU32> {
        (0..METER_MEMORY_WORDS).map(|_| AtomicU32::new(0)).collect()
    }

    #[test]
    fn readers_take_the_largest_peak_since_the_last_read() {
        let memory = memory();
        let writer = MeterWriter::new(&memory).unwrap();
        let reader = MeterReader::new(&memory).unwrap();
        writer.set_nodes(&[NodeId(3), NodeId(9)]);
        writer.record(0, [0.25, -0.5]);
        writer.record(0, [0.75, 0.1]);
        writer.record(1, [f32::NAN, f32::INFINITY]);
        assert_eq!(
            reader.take(),
            Some(vec![
                NodeMeter {
                    node: NodeId(3),
                    peaks: [0.75, 0.5],
                },
                NodeMeter {
                    node: NodeId(9),
                    peaks: [0.0, 0.0],
                },
            ])
        );
        let again = reader.take().unwrap();
        assert_eq!(again[0].peaks, [0.0, 0.0]);
    }

    #[test]
    fn nothing_is_read_before_the_writer_starts_or_while_the_table_changes() {
        let memory = memory();
        let reader = MeterReader::new(&memory).unwrap();
        assert_eq!(reader.take(), None, "no magic yet");

        let _first_writer = MeterWriter::new(&memory).unwrap();
        assert_eq!(reader.take(), Some(Vec::new()));
        // 模拟写入者在改节点表时退出：代数停在奇数
        memory[GENERATION_WORD].fetch_add(1, Ordering::SeqCst);
        assert_eq!(reader.take(), None);
        // 新的写入者接手后恢复
        let writer = MeterWriter::new(&memory).unwrap();
        writer.set_nodes(&[NodeId(1)]);
        assert_eq!(reader.take().map(|meters| meters.len()), Some(1));
    }

    #[test]
    fn a_new_node_table_resets_the_peaks() {
        let memory = memory();
        let writer = MeterWriter::new(&memory).unwrap();
        writer.set_nodes(&[NodeId(1)]);
        writer.record(0, [0.9, 0.9]);
        writer.set_nodes(&[NodeId(2)]);
        let reader = MeterReader::new(&memory).unwrap();
        assert_eq!(
            reader.take(),
            Some(vec![NodeMeter {
                node: NodeId(2),
                peaks: [0.0, 0.0],
            }])
        );
    }

    #[test]
    fn rejects_memory_that_is_too_small() {
        let small: Vec<AtomicU32> = (0..HEADER_WORDS).map(|_| AtomicU32::new(0)).collect();
        assert_eq!(MeterReader::new(&small).err(), Some(TooSmall(HEADER_WORDS)));
        assert!(MeterWriter::new(&small).is_err());
    }
}
