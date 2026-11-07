//! 外部设备节点在引擎一侧的端口：与设备 IO 线程（`aemeath-io`）之间的无锁环形缓冲的一端。
//!
//! 只在调度线程中读写，不分配内存、不加锁。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rtrb::{Consumer, Producer};

/// 输入端口积压超过目标水位的这么多倍时，丢掉多余的部分（例如调度线程曾被耽搁）
const BACKLOG_LIMIT_FACTOR: usize = 4;

#[derive(Debug)]
pub enum Port {
    /// 外部输入：IO 线程写入、调度线程读出。先攒够 `prime_samples` 才开始读，读空后重新攒。
    Input {
        consumer: Consumer<f32>,
        prime_samples: usize,
        primed: bool,
    },
    /// 外部输出：调度线程写入、IO 线程读出。`active` 为 false（设备还没打开或离线）时不写。
    Output {
        producer: Producer<f32>,
        active: Arc<AtomicBool>,
    },
}

/// 一次读写中发生的问题，由调度线程计数并记录。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PortIssues {
    /// 输入读空、补了静音的次数
    pub underflows: u32,
    /// 输入积压过多、丢弃的样本数
    pub skipped_samples: usize,
    /// 输出写不下、丢弃的样本数
    pub dropped_samples: usize,
}

impl Port {
    pub fn input(consumer: Consumer<f32>, prime_samples: usize) -> Self {
        Self::Input {
            consumer,
            prime_samples,
            primed: false,
        }
    }

    pub fn output(producer: Producer<f32>, active: Arc<AtomicBool>) -> Self {
        Self::Output { producer, active }
    }

    /// 输入端口：读满 `buffer`（交错立体声）；还没攒够或读空时不足的部分是 0。
    pub fn pull(&mut self, buffer: &mut [f32], issues: &mut PortIssues) {
        let Self::Input {
            consumer,
            prime_samples,
            primed,
        } = self
        else {
            return;
        };
        if !*primed {
            if consumer.slots() < *prime_samples {
                return;
            }
            *primed = true;
        }
        let backlog = consumer.slots();
        let limit = *prime_samples * BACKLOG_LIMIT_FACTOR;
        if backlog > limit
            && let Ok(chunk) = consumer.read_chunk(backlog - *prime_samples)
        {
            issues.skipped_samples += chunk.len();
            chunk.commit_all();
        }

        let wanted = buffer.len();
        let available = consumer.slots().min(wanted);
        if let Ok(chunk) = consumer.read_chunk(available) {
            let (first, second) = chunk.as_slices();
            buffer[..first.len()].copy_from_slice(first);
            buffer[first.len()..first.len() + second.len()].copy_from_slice(second);
            chunk.commit_all();
        }
        if available < wanted {
            buffer[available..].fill(0.0);
            *primed = false;
            issues.underflows += 1;
        }
    }

    /// 输出端口：写入 `buffer`（交错立体声），写不下的部分丢弃。设备不在线时什么也不做。
    pub fn push(&mut self, buffer: &[f32], issues: &mut PortIssues) {
        let Self::Output { producer, active } = self else {
            return;
        };
        if !active.load(Ordering::Acquire) {
            return;
        }
        let writable = producer.slots().min(buffer.len());
        if let Ok(chunk) = producer.write_chunk_uninit(writable) {
            chunk.fill_from_iter(buffer.iter().copied());
        }
        issues.dropped_samples += buffer.len() - writable;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rtrb::RingBuffer;

    #[test]
    fn input_waits_until_primed_then_reprimes_after_running_dry() {
        let (mut producer, consumer) = RingBuffer::new(64);
        let mut port = Port::input(consumer, 8);
        let mut issues = PortIssues::default();
        let mut buffer = [9.0f32; 4];

        for value in 1..=6 {
            producer.push(value as f32).unwrap();
        }
        port.pull(&mut buffer, &mut issues);
        assert_eq!(buffer, [9.0; 4], "not primed yet, buffer untouched");

        for value in 7..=8 {
            producer.push(value as f32).unwrap();
        }
        port.pull(&mut buffer, &mut issues);
        assert_eq!(buffer, [1.0, 2.0, 3.0, 4.0]);
        port.pull(&mut buffer, &mut issues);
        assert_eq!(buffer, [5.0, 6.0, 7.0, 8.0]);
        port.pull(&mut buffer, &mut issues);
        assert_eq!(buffer, [0.0; 4]);
        assert_eq!(issues.underflows, 1);
    }

    #[test]
    fn input_skips_an_excessive_backlog() {
        let (mut producer, consumer) = RingBuffer::new(128);
        let mut port = Port::input(consumer, 8);
        let mut issues = PortIssues::default();
        for value in 0..40 {
            producer.push(value as f32).unwrap();
        }
        let mut buffer = [0.0f32; 4];
        port.pull(&mut buffer, &mut issues);
        assert_eq!(issues.skipped_samples, 32);
        assert_eq!(buffer, [32.0, 33.0, 34.0, 35.0]);
    }

    #[test]
    fn output_writes_nothing_while_the_device_is_not_open() {
        let (producer, consumer) = RingBuffer::new(6);
        let active = Arc::new(AtomicBool::new(false));
        let mut port = Port::output(producer, Arc::clone(&active));
        let mut issues = PortIssues::default();
        port.push(&[1.0; 4], &mut issues);
        assert_eq!((consumer.slots(), issues.dropped_samples), (0, 0));
        active.store(true, Ordering::Release);
        port.push(&[1.0; 4], &mut issues);
        assert_eq!(consumer.slots(), 4);
    }

    #[test]
    fn output_drops_what_does_not_fit() {
        let (producer, mut consumer) = RingBuffer::new(6);
        let mut port = Port::output(producer, Arc::new(AtomicBool::new(true)));
        let mut issues = PortIssues::default();
        port.push(&[1.0, 2.0, 3.0, 4.0], &mut issues);
        port.push(&[5.0, 6.0, 7.0, 8.0], &mut issues);
        assert_eq!(issues.dropped_samples, 2);
        assert_eq!(consumer.slots(), 6);
        assert_eq!(consumer.pop(), Ok(1.0));
    }
}
