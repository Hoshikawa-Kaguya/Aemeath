//! 主时钟跟随（S4）：让所有虚拟设备的时间线锁定到主时钟设备上。
//!
//! 每秒从主时钟设备的 [`ClockTap`] 取一次 `IAudioClock` 的位置与对应的 QPC（由那台设备的 IO 线程读取，
//! 不另开流：外部设备默认独占打开，第二条流打不开，ADR-0014）：
//! - 频率：对最近 30 个样本做"主时钟秒数 ~ QPC 秒数"的线性回归，斜率就是主时钟相对 QPC 的速率；
//! - 相位：时间线实际走过的微帧数与按主时钟应走过的微帧数之差，以 [`KP_PER_SECOND`] 的速度拉回（锁相环）。
//!   只锁频率时残余误差会累积成相位漂移；锁相位后虚拟设备不需要 ASRC（S4 实测相位误差 ±18 µs）。
//!
//! 设备重新打开后（读数的会话编号变了），位置从零开始，重新建立基准；期间时间线保持上一次的速率。
//! 时间线通过 [`ClockTarget`] 调整，所以本 crate 不依赖引擎。线程退出时把它交还调用方，供下一个跟随线程使用。

use std::collections::VecDeque;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::clock::{ClockReading, ClockTap};
use crate::device;

const MICROFRAMES_PER_SECOND: f64 = 8_000.0;
/// 回归窗口：最近 30 个样本（30 s）
const WINDOW: usize = 30;
/// 至少这么多个样本后才开始调整
const MIN_SAMPLES: usize = 5;
/// 相位误差的回拉速度（1/s）
pub const KP_PER_SECOND: f64 = 0.1;
/// 相位校正量上限（相对速率）
const MAX_PHASE_CORRECTION: f64 = 100e-6;
/// 新会话开始后跳过这么多个读数再建立基准，避开开流时的抖动
const WARM_UP_READINGS: u32 = 2;
/// 检查新读数和停止信号的间隔
const POLL: Duration = Duration::from_millis(100);
const LOG_PERIOD: Duration = Duration::from_secs(60);
const HNS_PER_SECOND: f64 = 1e7;
const PPM: f64 = 1e6;

/// 被跟随的时钟（引擎的设备时间线）。
pub trait ClockTarget: Send {
    /// 时刻 `hns`（QPC，100 ns）对应的时间线位置（微帧，含小数）
    fn position(&self, hns: i64) -> f64;
    /// 设定速率因子（1.0 与 QPC 同速），在 `now_hns` 处保持连续
    fn set_rate(&mut self, factor: f64, now_hns: i64);
}

/// 跟随线程。[`Follower::stop`] 结束线程并交还时间线。
pub struct Follower {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<Box<dyn ClockTarget>>,
}

impl Follower {
    /// `tap` 是主时钟设备的 IO 线程写入的读数快照。
    pub fn spawn(tap: ClockTap, target: Box<dyn ClockTarget>) -> io::Result<Self> {
        let stop = device::stop_flag();
        let thread_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("aemeath-clock-follow".to_owned())
            .spawn(move || run(&tap, target, &thread_stop))?;
        Ok(Self { stop, thread })
    }

    /// 结束跟随并交还时间线；线程异常退出时返回 None。
    pub fn stop(self) -> Option<Box<dyn ClockTarget>> {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.join().ok()
    }
}

/// 一个会话内的基准与回归窗口。
struct Lock {
    session: u64,
    skipped: u32,
    /// (主时钟位置, QPC, 锁定时的时间线位置)
    origin: Option<(u64, i64, f64)>,
    window: VecDeque<(f64, f64)>,
}

impl Lock {
    fn new(session: u64) -> Self {
        Self {
            session,
            skipped: 0,
            origin: None,
            window: VecDeque::with_capacity(WINDOW + 1),
        }
    }
}

// 跟随线程更新时间线的几条指令期间，调度线程要等它写完，所以它不能被普通线程抢占。
fn run(tap: &ClockTap, mut target: Box<dyn ClockTarget>, stop: &AtomicBool) -> Box<dyn ClockTarget> {
    if let Err(error) = aemeath_sys::thread_priority::raise_current_thread() {
        tracing::warn!(%error, "cannot raise the clock following thread priority");
    }
    tracing::info!("clock master following started");
    let mut lock: Option<Lock> = None;
    let mut last_reading: Option<ClockReading> = None;
    let mut last_log = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(POLL);
        let Some(reading) = tap.latest() else {
            continue;
        };
        if last_reading == Some(reading) {
            continue;
        }
        last_reading = Some(reading);
        let current = match &mut lock {
            Some(current) if current.session == reading.session => current,
            _ => lock.insert(Lock::new(reading.session)),
        };
        if current.skipped < WARM_UP_READINGS {
            current.skipped += 1;
            continue;
        }
        if let Some((rate, phase_error_seconds, factor)) = update(current, reading, target.as_mut())
            && last_log.elapsed() >= LOG_PERIOD
        {
            tracing::info!(
                master_ppm = format!("{:.2}", (rate - 1.0) * PPM),
                phase_error_us = format!("{:.1}", phase_error_seconds * PPM),
                applied_ppm = format!("{:.2}", (factor - 1.0) * PPM),
                "clock master following"
            );
            last_log = Instant::now();
        }
    }
    target
}

/// 加入一个读数；样本足够时调整时间线，返回 (主时钟速率, 相位误差秒, 实际设定的速率因子)。
fn update(lock: &mut Lock, reading: ClockReading, target: &mut dyn ClockTarget) -> Option<(f64, f64, f64)> {
    let frequency = reading.frequency.max(1) as f64;
    let (origin_position, origin_qpc, origin_microframe) =
        *lock
            .origin
            .get_or_insert((reading.position, reading.qpc, target.position(reading.qpc)));
    let master_seconds = reading.position.saturating_sub(origin_position) as f64 / frequency;
    let real_seconds = (reading.qpc - origin_qpc) as f64 / HNS_PER_SECOND;
    lock.window.push_back((real_seconds, master_seconds));
    if lock.window.len() > WINDOW {
        lock.window.pop_front();
    }
    if lock.window.len() < MIN_SAMPLES {
        return None;
    }
    let rate = slope(&lock.window);
    let desired = origin_microframe + master_seconds * MICROFRAMES_PER_SECOND;
    let phase_error_seconds = (target.position(reading.qpc) - desired) / MICROFRAMES_PER_SECOND;
    let correction = (-KP_PER_SECOND * phase_error_seconds).clamp(-MAX_PHASE_CORRECTION, MAX_PHASE_CORRECTION);
    let factor = rate * (1.0 + correction);
    target.set_rate(factor, aemeath_sys::clock::now_hns());
    Some((rate, phase_error_seconds, factor))
}

/// 最小二乘斜率 d(master)/d(real)
fn slope(window: &VecDeque<(f64, f64)>) -> f64 {
    let count = window.len() as f64;
    let (sum_x, sum_y) = window
        .iter()
        .fold((0.0, 0.0), |(x, y), (real, master)| (x + real, y + master));
    let (mean_x, mean_y) = (sum_x / count, sum_y / count);
    let (numerator, denominator) = window.iter().fold((0.0, 0.0), |(num, den), (real, master)| {
        (
            num + (real - mean_x) * (master - mean_y),
            den + (real - mean_x) * (real - mean_x),
        )
    });
    if denominator > 0.0 {
        numerator / denominator
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slope_of_a_faster_clock() {
        let window: VecDeque<(f64, f64)> = (0..30)
            .map(|second| (f64::from(second), f64::from(second) * (1.0 + 50e-6)))
            .collect();
        assert!(((slope(&window) - 1.0) * PPM - 50.0).abs() < 1e-6);
    }

    /// 以 QPC 速率前进的时间线
    struct FakeTimeline {
        factor: f64,
    }

    impl ClockTarget for FakeTimeline {
        fn position(&self, hns: i64) -> f64 {
            hns as f64 / HNS_PER_SECOND * MICROFRAMES_PER_SECOND * self.factor
        }

        fn set_rate(&mut self, factor: f64, _now_hns: i64) {
            self.factor = factor;
        }
    }

    #[test]
    fn follows_a_faster_master_after_enough_readings() {
        // IAudioClock 的频率通常是 10 MHz；用设备帧率会让位置取整的误差盖过 0.5 ppm 的容差
        const FREQUENCY: u64 = 10_000_000;
        let master_rate = 1.0 + 40e-6;
        let mut lock = Lock::new(1);
        let mut timeline = FakeTimeline { factor: 1.0 };
        let mut applied = None;
        for second in 0..10_i64 {
            let reading = ClockReading {
                session: 1,
                position: (second as f64 * FREQUENCY as f64 * master_rate) as u64,
                frequency: FREQUENCY,
                qpc: second * HNS_PER_SECOND as i64,
            };
            applied = update(&mut lock, reading, &mut timeline);
        }
        let (rate, _, _) = applied.unwrap();
        assert!(((rate - 1.0) * PPM - 40.0).abs() < 0.5, "{rate}");
    }
}
