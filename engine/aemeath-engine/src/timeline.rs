//! 速率可调的设备时间线：所有虚拟设备的时钟。
//!
//! 时间线把 QPC 时刻映射到 USB 微帧（125 µs）。映射是分段线性的：每次调整速率都在"当前时刻"
//! 重新设锚点，所以时间线连续，只是斜率改变。主时钟跟随线程调整速率，让虚拟设备锁定在主时钟设备上。
//!
//! 线程模型：读者是调度线程（实时），写者只有一个（跟随线程，持有 [`TimelineWriter`]）。
//! 用顺序锁实现：读者不加锁、不分配内存，写入期间读到的不完整数据会被丢弃重读（实时线程不加锁）。
//! 数据本身放在原子变量里，所以整个实现是安全 Rust。

use std::hint;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering, fence};

use aemeath_dsp::resample::MAX_CORRECTION_PPM;

/// 高速 USB 微帧的标称长度，单位 100 ns
pub const MICROFRAME_HNS: f64 = 1_250.0;
/// 每毫秒的微帧数
pub const MICROFRAMES_PER_MS: i64 = 8;
/// 速率偏离 QPC 的上限：外部设备的 ASRC 最多只能校正这么多
const MAX_RATE_DEVIATION: f64 = MAX_CORRECTION_PPM * 1e-6;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Segment {
    anchor_hns: i64,
    anchor_microframe: f64,
    hns_per_microframe: f64,
}

/// 虚拟设备共用的时间线：把 QPC 时刻换算成微帧编号。一个线程写（主时钟跟随），任意线程读。
#[derive(Debug)]
pub struct Timeline {
    /// 奇数表示正在写入
    sequence: AtomicU64,
    anchor_hns: AtomicI64,
    anchor_microframe: AtomicU64,
    hns_per_microframe: AtomicU64,
}

/// 时间线唯一的写入句柄。不能克隆，保证只有一个写者。
#[derive(Debug)]
pub struct TimelineWriter {
    timeline: Arc<Timeline>,
}

impl Timeline {
    /// 以 `start_hns` 为微帧 0，速率与 QPC 相同。
    pub fn new(start_hns: i64) -> (Arc<Self>, TimelineWriter) {
        let timeline = Arc::new(Self {
            sequence: AtomicU64::new(0),
            anchor_hns: AtomicI64::new(start_hns),
            anchor_microframe: AtomicU64::new(0f64.to_bits()),
            hns_per_microframe: AtomicU64::new(MICROFRAME_HNS.to_bits()),
        });
        let writer = TimelineWriter {
            timeline: Arc::clone(&timeline),
        };
        (timeline, writer)
    }

    /// 读出一份一致的当前分段：读的过程中遇到写入就重读。
    fn load(&self) -> Segment {
        loop {
            let before = self.sequence.load(Ordering::Acquire);
            if before % 2 == 1 {
                hint::spin_loop();
                continue;
            }
            let segment = Segment {
                anchor_hns: self.anchor_hns.load(Ordering::Relaxed),
                anchor_microframe: f64::from_bits(self.anchor_microframe.load(Ordering::Relaxed)),
                hns_per_microframe: f64::from_bits(self.hns_per_microframe.load(Ordering::Relaxed)),
            };
            fence(Ordering::Acquire);
            if self.sequence.load(Ordering::Relaxed) == before {
                return segment;
            }
        }
    }

    /// 时刻 `hns` 在时间线上的位置（微帧，含小数）。
    pub fn position(&self, hns: i64) -> f64 {
        let segment = self.load();
        segment.anchor_microframe + (hns - segment.anchor_hns) as f64 / segment.hns_per_microframe
    }

    /// 时刻 `hns` 所在的微帧（向下取整）。
    pub fn microframe_at(&self, hns: i64) -> i64 {
        self.position(hns).floor() as i64
    }

    /// 微帧 `microframe` 开始的时刻，单位 100 ns。
    pub fn time_of(&self, microframe: i64) -> i64 {
        let segment = self.load();
        let offset = (microframe as f64 - segment.anchor_microframe) * segment.hns_per_microframe;
        segment.anchor_hns + offset.round() as i64
    }

    /// 当前速率相对 QPC 的偏差，单位 ppm。
    pub fn rate_ppm(&self) -> f64 {
        const PPM: f64 = 1e6;
        (MICROFRAME_HNS / self.load().hns_per_microframe - 1.0) * PPM
    }
}

impl TimelineWriter {
    /// 设定速率因子（1.0 与 QPC 同速；1 + 50e-6 表示快 50 ppm），在 `now_hns` 处保持连续。
    ///
    /// 限制在其他设备的 ASRC 能跟上的范围内；非有限值忽略。
    pub fn set_rate(&self, factor: f64, now_hns: i64) {
        if !factor.is_finite() {
            return;
        }
        let factor = factor.clamp(1.0 - MAX_RATE_DEVIATION, 1.0 + MAX_RATE_DEVIATION);
        let timeline = &self.timeline;
        let current = timeline.load();
        let microframe_now =
            current.anchor_microframe + (now_hns - current.anchor_hns) as f64 / current.hns_per_microframe;

        timeline.sequence.fetch_add(1, Ordering::Relaxed);
        fence(Ordering::Release);
        timeline.anchor_hns.store(now_hns, Ordering::Relaxed);
        timeline
            .anchor_microframe
            .store(microframe_now.to_bits(), Ordering::Relaxed);
        timeline
            .hns_per_microframe
            .store((MICROFRAME_HNS / factor).to_bits(), Ordering::Relaxed);
        timeline.sequence.fetch_add(1, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: i64 = 1_000_000;

    #[test]
    fn maps_time_to_microframes_at_nominal_rate() {
        let (timeline, _) = Timeline::new(START);
        assert_eq!(timeline.microframe_at(START), 0);
        assert_eq!(timeline.microframe_at(START + 1_249), 0);
        assert_eq!(timeline.microframe_at(START + 1_250), 1);
        assert_eq!(timeline.time_of(8), START + 10_000);
        assert_eq!(timeline.rate_ppm(), 0.0);
    }

    #[test]
    fn rate_changes_keep_the_timeline_continuous() {
        let (timeline, writer) = Timeline::new(START);
        let switch = START + 80_000; // 第 64 个微帧
        writer.set_rate(1.0 + 100e-6, switch);
        assert_eq!(timeline.microframe_at(switch), 64);
        assert!((timeline.rate_ppm() - 100.0).abs() < 1e-6);
        // 之后 1 秒多走 0.8 个微帧（8000 × 100 ppm）
        let one_second_later = timeline.microframe_at(switch + 10_000_000);
        assert_eq!(one_second_later, 64 + 8_000);
        assert!(timeline.time_of(64 + 8_000) < switch + 10_000_000);
    }

    #[test]
    fn the_rate_stays_within_what_the_asrc_can_follow() {
        let (timeline, writer) = Timeline::new(START);
        writer.set_rate(1.05, START);
        assert!((timeline.rate_ppm() - 1000.0).abs() < 1e-3, "{}", timeline.rate_ppm());
        writer.set_rate(0.0, START);
        assert!((timeline.rate_ppm() + 1000.0).abs() < 1e-3, "{}", timeline.rate_ppm());
        writer.set_rate(f64::NAN, START);
        writer.set_rate(f64::INFINITY, START);
        assert!(
            (timeline.rate_ppm() + 1000.0).abs() < 1e-3,
            "non-finite rates are ignored"
        );
    }

    #[test]
    fn concurrent_reads_never_see_a_torn_segment() {
        let (timeline, writer) = Timeline::new(0);
        let reader = Arc::clone(&timeline);
        // 速率保持 1.0、只移动锚点：每一份完整的段都把 t = 1 s 映射到第 8000 个微帧。
        // 锚点之间相差 100 ms，读到新旧混合的锚点时，结果会偏离 800 个微帧的整数倍。
        let handle = std::thread::spawn(move || {
            for _ in 0..200_000 {
                assert_eq!(reader.microframe_at(10_000_000), 8_000);
            }
        });
        const ANCHOR_SPACING_HNS: i64 = 1_000_000;
        const ANCHOR_COUNT: i64 = 50;
        for step in 0..20_000i64 {
            writer.set_rate(1.0, (step % ANCHOR_COUNT) * ANCHOR_SPACING_HNS);
        }
        handle.join().unwrap();
    }
}
