//! 交错立体声帧的环形缓冲。
//!
//! 只在调度线程内使用（单线程，不需要同步）。容量在创建时一次分配，之后的读写都不分配内存。

use crate::STEREO;

/// 定长的交错立体声帧队列，满了丢最旧的帧。
#[derive(Debug, Clone)]
pub struct FrameFifo {
    samples: Vec<f32>,
    capacity_frames: usize,
    /// 下一个可读帧的位置（帧）
    read: usize,
    len_frames: usize,
}

impl FrameFifo {
    pub fn with_capacity(capacity_frames: usize) -> Self {
        Self {
            samples: vec![0.0; capacity_frames * STEREO],
            capacity_frames,
            read: 0,
            len_frames: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.len_frames
    }

    pub fn is_empty(&self) -> bool {
        self.len_frames == 0
    }

    /// 清空队列，不释放内存。
    pub fn clear(&mut self) {
        self.read = 0;
        self.len_frames = 0;
    }

    /// 追加交错的帧。放不下时丢弃最旧的帧，返回丢弃的帧数。
    pub fn push(&mut self, interleaved: &[f32]) -> usize {
        let frames = interleaved.len() / STEREO;
        if self.capacity_frames == 0 {
            return frames;
        }
        // 比容量还多时只保留最后一段
        let skip = frames.saturating_sub(self.capacity_frames);
        let incoming = &interleaved[skip * STEREO..frames * STEREO];
        let incoming_frames = frames - skip;
        let overflow = (self.len_frames + incoming_frames).saturating_sub(self.capacity_frames);
        self.skip(overflow);

        // 按回绕点分两段整块拷贝，比逐帧取模快一个数量级（调度线程上每轮每条流都要走）
        let write = (self.read + self.len_frames) % self.capacity_frames;
        let first = incoming_frames.min(self.capacity_frames - write);
        let (head, tail) = incoming.split_at(first * STEREO);
        self.samples[write * STEREO..(write + first) * STEREO].copy_from_slice(head);
        self.samples[..tail.len()].copy_from_slice(tail);
        self.len_frames += incoming_frames;
        skip + overflow
    }

    /// 丢掉最旧的至多 `frames` 帧（只移动读位置），返回实际丢掉的帧数。
    pub fn skip(&mut self, frames: usize) -> usize {
        let skipped = frames.min(self.len_frames);
        if skipped > 0 {
            self.read = (self.read + skipped) % self.capacity_frames;
            self.len_frames -= skipped;
        }
        skipped
    }

    /// 取出最多 `out.len() / STEREO` 帧写入 `out`，不足的部分填 0。返回实际取出的帧数。
    pub fn pop_into(&mut self, out: &mut [f32]) -> usize {
        let wanted = out.len() / STEREO;
        let available = wanted.min(self.len_frames);
        if available > 0 {
            let first = available.min(self.capacity_frames - self.read);
            let (head, tail) = out[..available * STEREO].split_at_mut(first * STEREO);
            head.copy_from_slice(&self.samples[self.read * STEREO..(self.read + first) * STEREO]);
            tail.copy_from_slice(&self.samples[..tail.len()]);
            self.read = (self.read + available) % self.capacity_frames;
            self.len_frames -= available;
        }
        out[available * STEREO..].fill(0.0);
        available
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(values: &[f32]) -> Vec<f32> {
        values.iter().flat_map(|value| [*value, -*value]).collect()
    }

    #[test]
    fn pops_in_order_across_the_wrap_point() {
        let mut fifo = FrameFifo::with_capacity(4);
        fifo.push(&frames(&[1.0, 2.0, 3.0]));
        let mut out = vec![0.0; 4];
        assert_eq!(fifo.pop_into(&mut out), 2);
        assert_eq!(out, frames(&[1.0, 2.0]));
        fifo.push(&frames(&[4.0, 5.0, 6.0]));
        let mut out = vec![0.0; 8];
        assert_eq!(fifo.pop_into(&mut out), 4);
        assert_eq!(out, frames(&[3.0, 4.0, 5.0, 6.0]));
    }

    #[test]
    fn underflow_fills_zeros() {
        let mut fifo = FrameFifo::with_capacity(4);
        fifo.push(&frames(&[1.0]));
        let mut out = vec![9.0; 6];
        assert_eq!(fifo.pop_into(&mut out), 1);
        assert_eq!(out, frames(&[1.0, 0.0, 0.0]));
    }

    #[test]
    fn overflow_drops_the_oldest_frames() {
        let mut fifo = FrameFifo::with_capacity(3);
        fifo.push(&frames(&[1.0, 2.0]));
        assert_eq!(fifo.push(&frames(&[3.0, 4.0])), 1);
        let mut out = vec![0.0; 6];
        fifo.pop_into(&mut out);
        assert_eq!(out, frames(&[2.0, 3.0, 4.0]));
        assert_eq!(fifo.push(&frames(&[5.0, 6.0, 7.0, 8.0, 9.0])), 2);
        assert_eq!(fifo.len(), 3);
    }

    #[test]
    fn a_write_larger_than_the_capacity_keeps_the_newest_frames_in_order() {
        let mut fifo = FrameFifo::with_capacity(4);
        fifo.push(&frames(&[1.0, 2.0, 3.0]));
        fifo.pop_into(&mut [0.0; 4]);
        assert_eq!(fifo.push(&frames(&[4.0, 5.0, 6.0, 7.0, 8.0, 9.0])), 3);
        let mut out = vec![0.0; 8];
        assert_eq!(fifo.pop_into(&mut out), 4);
        assert_eq!(out, frames(&[6.0, 7.0, 8.0, 9.0]));
    }

    #[test]
    fn skip_drops_the_oldest_frames_across_the_wrap_point() {
        let mut fifo = FrameFifo::with_capacity(4);
        fifo.push(&frames(&[1.0, 2.0, 3.0]));
        fifo.pop_into(&mut [0.0; 4]);
        fifo.push(&frames(&[4.0, 5.0, 6.0]));
        assert_eq!(fifo.skip(3), 3);
        let mut out = vec![0.0; 4];
        assert_eq!(fifo.pop_into(&mut out), 1);
        assert_eq!(out, frames(&[6.0, 0.0]));
        assert_eq!(fifo.skip(5), 0, "nothing left to skip");
        assert_eq!(FrameFifo::with_capacity(0).skip(1), 0);
    }
}
