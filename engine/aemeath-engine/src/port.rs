//! 外部设备节点在引擎一侧的端口：与设备 IO 线程（`aemeath-io`）之间的无锁环形缓冲的一端。
//!
//! 每次开始收发（输入攒够水位、输出的设备刚打开）都从 0 淡入：设备流重建（改了独占设置、格式、
//! 重采样算法）或者设备重新连接后，声音不会以满幅度突然出现（切换时不爆音）。
//!
//! 只在调度线程中读写，不分配内存、不加锁。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rtrb::{Consumer, Producer};

use crate::STEREO;

/// 开始收发时的线性淡入（只在端口内部使用）。
#[derive(Debug, Clone, Copy)]
pub struct Onset {
    total: u32,
    remaining: u32,
}

impl Onset {
    fn new(total: u32) -> Self {
        Self { total, remaining: 0 }
    }

    fn restart(&mut self) {
        self.remaining = self.total;
    }

    /// 这一帧的增益，并前进一帧。
    fn next_gain(&mut self) -> f32 {
        if self.remaining == 0 {
            return 1.0;
        }
        self.remaining -= 1;
        1.0 - self.remaining as f32 / self.total.max(1) as f32
    }
}

/// 混音器与外部设备、应用源之间的一个端口，两端在不同线程，只经无锁环形缓冲交换音频。
#[derive(Debug)]
pub enum Port {
    /// 外部输入：IO 线程写入、调度线程读出。先攒够 `prime_samples` 才开始读，读空后重新攒。
    Input {
        consumer: Consumer<f32>,
        prime_samples: usize,
        primed: bool,
        onset: Onset,
    },
    /// 外部输出：调度线程写入、IO 线程读出。`active` 为 false（设备还没打开或离线）时不写。
    Output {
        producer: Producer<f32>,
        active: Arc<AtomicBool>,
        /// 上一次写入时设备是否在线，用来发现"刚打开"
        was_active: bool,
        onset: Onset,
    },
    /// 应用源：程序的每个根进程一条输入，求和。
    /// 进程退出或还没启动时对应的输入是空的，不算欠载。
    Summed { inputs: Vec<Port>, scratch: Vec<f32> },
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
    /// `fade_frames`：每次开始收发时淡入的帧数（引擎采样率下的 10 ms）
    pub fn input(consumer: Consumer<f32>, prime_samples: usize, fade_frames: u32) -> Self {
        Self::Input {
            consumer,
            prime_samples,
            primed: false,
            onset: Onset::new(fade_frames),
        }
    }

    pub fn output(producer: Producer<f32>, active: Arc<AtomicBool>, fade_frames: u32) -> Self {
        Self::Output {
            producer,
            active,
            was_active: false,
            onset: Onset::new(fade_frames),
        }
    }

    /// `scratch_samples`：一块最多的样本数，一次分配好，调度线程里不再分配。
    pub fn summed(inputs: Vec<Port>, scratch_samples: usize) -> Self {
        Self::Summed {
            inputs,
            scratch: vec![0.0; scratch_samples],
        }
    }

    /// 输入端口：读满 `buffer`（交错立体声）；还没攒够或读空时不足的部分是 0。
    pub fn pull(&mut self, buffer: &mut [f32], issues: &mut PortIssues) {
        if let Self::Summed { inputs, scratch } = self {
            let scratch = &mut scratch[..buffer.len()];
            buffer.fill(0.0);
            for input in inputs {
                scratch.fill(0.0);
                input.pull(scratch, &mut PortIssues::default());
                for (out, sample) in buffer.iter_mut().zip(scratch.iter()) {
                    *out += sample;
                }
            }
            return;
        }
        let Self::Input {
            consumer,
            prime_samples,
            primed,
            onset,
        } = self
        else {
            return;
        };
        if !*primed {
            if consumer.slots() < *prime_samples {
                return;
            }
            *primed = true;
            onset.restart();
        }
        issues.skipped_samples += aemeath_io::skip_backlog(consumer, *prime_samples);

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
        for frame in buffer[..available].as_chunks_mut::<STEREO>().0 {
            let gain = onset.next_gain();
            frame[0] *= gain;
            frame[1] *= gain;
        }
    }

    /// 输出端口：写入 `buffer`（交错立体声），写不下的部分丢弃。设备不在线时什么也不做。
    pub fn push(&mut self, buffer: &[f32], issues: &mut PortIssues) {
        let Self::Output {
            producer,
            active,
            was_active,
            onset,
        } = self
        else {
            return;
        };
        let is_active = active.load(Ordering::Acquire);
        if is_active && !*was_active {
            onset.restart();
        }
        *was_active = is_active;
        if !is_active {
            return;
        }
        let writable = producer.slots().min(buffer.len());
        if let Ok(chunk) = producer.write_chunk_uninit(writable) {
            let mut gain = 1.0;
            chunk.fill_from_iter(buffer.iter().enumerate().map(|(index, sample)| {
                if index % STEREO == 0 {
                    gain = onset.next_gain();
                }
                sample * gain
            }));
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
        let mut port = Port::input(consumer, 8, 0);
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
        let mut port = Port::input(consumer, 8, 0);
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
        let mut port = Port::output(producer, Arc::clone(&active), 0);
        let mut issues = PortIssues::default();
        port.push(&[1.0; 4], &mut issues);
        assert_eq!((consumer.slots(), issues.dropped_samples), (0, 0));
        active.store(true, Ordering::Release);
        port.push(&[1.0; 4], &mut issues);
        assert_eq!(consumer.slots(), 4);
    }

    #[test]
    fn input_and_output_fade_in_when_they_start() {
        let (mut producer, consumer) = RingBuffer::new(64);
        let mut port = Port::input(consumer, 8, 4);
        let mut issues = PortIssues::default();
        for _ in 0..8 {
            producer.push(1.0).unwrap();
        }
        let mut buffer = [0.0f32; 8];
        port.pull(&mut buffer, &mut issues);
        assert_eq!(buffer, [0.25, 0.25, 0.5, 0.5, 0.75, 0.75, 1.0, 1.0]);

        let (producer, mut consumer) = RingBuffer::new(16);
        let active = Arc::new(AtomicBool::new(true));
        let mut port = Port::output(producer, Arc::clone(&active), 2);
        port.push(&[1.0; 6], &mut issues);
        let written: Vec<f32> = std::iter::from_fn(|| consumer.pop().ok()).collect();
        assert_eq!(written, vec![0.5, 0.5, 1.0, 1.0, 1.0, 1.0]);
        // 设备离线再上线：重新淡入
        active.store(false, Ordering::Release);
        port.push(&[1.0; 2], &mut issues);
        active.store(true, Ordering::Release);
        port.push(&[1.0; 2], &mut issues);
        assert_eq!(consumer.pop(), Ok(0.5));
    }

    #[test]
    fn summed_inputs_add_up_and_an_empty_input_is_silent() {
        let (mut first_producer, first) = RingBuffer::new(16);
        let (mut second_producer, second) = RingBuffer::new(16);
        let (_idle_producer, idle) = RingBuffer::<f32>::new(16);
        let inputs = vec![
            Port::input(first, 2, 0),
            Port::input(second, 2, 0),
            Port::input(idle, 2, 0),
        ];
        let mut port = Port::summed(inputs, 8);
        for _ in 0..4 {
            first_producer.push(0.25).unwrap();
            second_producer.push(0.5).unwrap();
        }
        let mut issues = PortIssues::default();
        let mut buffer = [9.0f32; 4];
        port.pull(&mut buffer, &mut issues);
        assert_eq!(buffer, [0.75; 4]);
        port.pull(&mut buffer, &mut issues);
        assert_eq!(buffer, [0.0; 4]);
        assert_eq!(
            issues,
            PortIssues::default(),
            "a process that exited is not an underflow"
        );
    }

    #[test]
    fn output_drops_what_does_not_fit() {
        let (producer, mut consumer) = RingBuffer::new(6);
        let mut port = Port::output(producer, Arc::new(AtomicBool::new(true)), 0);
        let mut issues = PortIssues::default();
        port.push(&[1.0, 2.0, 3.0, 4.0], &mut issues);
        port.push(&[5.0, 6.0, 7.0, 8.0], &mut issues);
        assert_eq!(issues.dropped_samples, 2);
        assert_eq!(consumer.slots(), 6);
        assert_eq!(consumer.pop(), Ok(1.0));
    }
}
