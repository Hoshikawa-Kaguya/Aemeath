//! 桥接线程共用的错误类型与统计日志。
//!
//! 长时间运行出问题时要靠日志排查：每分钟记一次缓冲占用、漂移估计和校正量的摘要；溢出、欠载立即记录（限频）。
//! 每秒的平均占用另外写进 [`BufferLevel`]，供标题栏显示。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use aemeath_dsp::resample::{RateUpdate, ResampleError};
use aemeath_protocol::message::OfflineReason;
use wasapi::WasapiError;

use crate::device;

const MICROSECONDS_PER_MS: f64 = 1000.0;

/// 一个桥接的 ASRC 缓冲最近一秒的平均占用与目标占用，IO 线程写、控制线程读。
/// 两个值（µs）打包在一个原子量里，读到的总是同一次写入的一对。
#[derive(Debug, Default)]
pub struct BufferLevel(AtomicU64);

impl BufferLevel {
    fn publish(&self, level_ms: f64, target_ms: f64) {
        let to_us = |ms: f64| (ms * MICROSECONDS_PER_MS).clamp(0.0, f64::from(u32::MAX)) as u64;
        self.0
            .store((to_us(target_ms) << 32) | to_us(level_ms), Ordering::Relaxed);
    }

    fn clear(&self) {
        self.0.store(0, Ordering::Relaxed);
    }

    /// (平均占用 ms, 目标占用 ms)；没有正在运行的会话、或会话还没满一秒时为 None。
    pub fn read(&self) -> Option<(f32, f32)> {
        let packed = self.0.load(Ordering::Relaxed);
        let target_us = packed >> 32;
        if target_us == 0 {
            return None;
        }
        let level_us = packed & u64::from(u32::MAX);
        let to_ms = |us: u64| (us as f64 / MICROSECONDS_PER_MS) as f32;
        Some((to_ms(level_us), to_ms(target_us)))
    }
}

/// 控制器的调整周期
pub const CONTROL_PERIOD: Duration = Duration::from_secs(1);
/// 摘要日志的间隔
const LOG_PERIOD: Duration = Duration::from_secs(60);

/// 会话中的错误：设备错误要重试，重采样器错误同样关闭这次会话后重试。
#[derive(Debug)]
pub enum BridgeError {
    Device(WasapiError),
    Resample(ResampleError),
    /// 长时间没有设备事件（见 [`device::EventWatch`]）
    Stalled,
}

impl From<device::WaitError> for BridgeError {
    fn from(error: device::WaitError) -> Self {
        match error {
            device::WaitError::Device(error) => Self::Device(error),
            device::WaitError::Stalled => Self::Stalled,
        }
    }
}

impl From<WasapiError> for BridgeError {
    fn from(error: WasapiError) -> Self {
        Self::Device(error)
    }
}

impl From<ResampleError> for BridgeError {
    fn from(error: ResampleError) -> Self {
        Self::Resample(error)
    }
}

/// 桥接的统计与日志：每秒给控制器一个平均缓冲占用，每分钟记一次摘要；溢出、欠载立即记录（限频）。
pub struct BridgeLog {
    endpoint: String,
    session_started: Instant,
    last_control: Instant,
    last_log: Instant,
    level_sum: f64,
    level_count: u64,
    level_min: f64,
    level_max: f64,
    pub discontinuities: u64,
    overflow_frames: u64,
    underflow_events: u64,
    /// 本摘要周期内单块重采样的最大耗时
    block_max: Duration,
    /// 整个会话内单块重采样的最大耗时
    session_block_max: Duration,
    last_problem_log: Option<Instant>,
    last_update: RateUpdate,
    target_ms: f64,
    shared: Arc<BufferLevel>,
}

/// 溢出、欠载的日志最多每隔这么久记一次
const PROBLEM_LOG_INTERVAL: Duration = Duration::from_secs(5);

impl BridgeLog {
    /// 为一个端点开始一段新的统计；`endpoint` 只用于日志里区分设备，`target_ms` 是这次会话的目标占用。
    pub fn new(endpoint: &str, target_ms: f64, shared: Arc<BufferLevel>) -> Self {
        let now = Instant::now();
        Self {
            endpoint: endpoint.to_owned(),
            session_started: now,
            last_control: now,
            last_log: now,
            level_sum: 0.0,
            level_count: 0,
            level_min: f64::MAX,
            level_max: 0.0,
            discontinuities: 0,
            overflow_frames: 0,
            underflow_events: 0,
            block_max: Duration::ZERO,
            session_block_max: Duration::ZERO,
            last_problem_log: None,
            last_update: RateUpdate::default(),
            target_ms,
            shared,
        }
    }

    /// 记录一块重采样的耗时。
    pub fn block_processed(&mut self, elapsed: Duration) {
        self.block_max = self.block_max.max(elapsed);
        self.session_block_max = self.session_block_max.max(elapsed);
    }

    /// 记录一次环形缓冲的占用（ms），攒到下一次控制时取平均。
    pub fn level(&mut self, level_ms: f64) {
        self.level_sum += level_ms;
        self.level_count += 1;
        self.level_min = self.level_min.min(level_ms);
        self.level_max = self.level_max.max(level_ms);
    }

    /// 是否该调整一次重采样比率：距上次已满一个控制周期，并且这期间记录过缓冲占用。
    pub fn control_due(&self) -> bool {
        self.last_control.elapsed() >= CONTROL_PERIOD && self.level_count > 0
    }

    /// 取出这个控制周期的平均缓冲占用（ms），并开始下一个周期。平均能滤掉设备按块交付造成的锯齿。
    pub fn take_average(&mut self) -> f64 {
        let average = self.level_sum / self.level_count.max(1) as f64;
        self.level_sum = 0.0;
        self.level_count = 0;
        self.last_control = Instant::now();
        self.shared.publish(average, self.target_ms);
        average
    }

    /// 记下控制器这次的输出；每满一个日志周期写一条 `bridge status` 摘要，并清零周期内的统计。
    pub fn after_control(&mut self, update: RateUpdate) {
        self.last_update = update;
        if self.last_log.elapsed() >= LOG_PERIOD {
            tracing::info!(
                endpoint = %self.endpoint,
                level_min_ms = format!("{:.2}", self.level_min),
                level_max_ms = format!("{:.2}", self.level_max),
                error_ms = format!("{:.2}", update.error_ms),
                drift_ppm = format!("{:.2}", update.integral_ppm),
                correction_ppm = format!("{:.2}", update.correction_ppm),
                overflow_frames = self.overflow_frames,
                underflow_events = self.underflow_events,
                discontinuities = self.discontinuities,
                block_max_us = self.block_max.as_micros() as u64,
                "bridge status"
            );
            self.last_log = Instant::now();
            self.level_min = f64::MAX;
            self.level_max = 0.0;
            self.block_max = Duration::ZERO;
        }
    }

    /// 环形缓冲满了，丢掉了 `frames` 帧。计数并限频记警告。
    pub fn overflow(&mut self, frames: usize) {
        self.overflow_frames += frames as u64;
        self.problem("ring buffer overflow, frames dropped");
    }

    /// 环形缓冲空了，补了静音。计数并限频记警告。
    pub fn underflow(&mut self) {
        self.underflow_events += 1;
        self.problem("ring buffer underflow, silence inserted");
    }

    fn problem(&mut self, message: &str) {
        if self
            .last_problem_log
            .is_none_or(|last| last.elapsed() >= PROBLEM_LOG_INTERVAL)
        {
            tracing::warn!(
                endpoint = %self.endpoint,
                overflow_frames = self.overflow_frames,
                underflow_events = self.underflow_events,
                drift_ppm = format!("{:.2}", self.last_update.integral_ppm),
                "{message}"
            );
            self.last_problem_log = Some(Instant::now());
        }
    }
}

impl BridgeError {
    /// 归为离线原因，供状态显示
    pub fn offline_reason(&self) -> OfflineReason {
        match self {
            Self::Device(error) => device::offline_reason(error),
            Self::Resample(_) => OfflineReason::WaitingForDevice,
            Self::Stalled => OfflineReason::NoSignal,
        }
    }
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Device(error) => write!(f, "device error: {error}"),
            Self::Resample(error) => write!(f, "resampler error: {error}"),
            Self::Stalled => write!(f, "no device events for several seconds"),
        }
    }
}

impl Drop for BridgeLog {
    /// 会话结束（设备被拔出、拓扑不再使用、程序退出）时记录这次会话的汇总。
    fn drop(&mut self) {
        self.shared.clear();
        tracing::info!(
            endpoint = %self.endpoint,
            seconds = format!("{:.1}", self.session_started.elapsed().as_secs_f64()),
            drift_ppm = format!("{:.2}", self.last_update.integral_ppm),
            overflow_frames = self.overflow_frames,
            underflow_events = self.underflow_events,
            discontinuities = self.discontinuities,
            block_max_us = self.session_block_max.as_micros() as u64,
            "bridge session ended"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy)]
    enum Side {
        /// 设备写入环形缓冲、引擎读出（外部输入、应用源）
        Capture,
        /// 引擎写入、设备取走（外部输出）
        Render,
    }

    /// 模拟一个桥接 10 s：引擎每 1 ms（有漂移时略长或略短）收发 1 ms 的音频，设备每 `period_ms` 收发一包。
    /// 返回 (真实的时间平均, 只在设备收发后记录的平均, 收发前后各记一次的平均)，单位 ms。
    fn simulate(side: Side, period_ms: f64, phase_ms: f64, drift_ppm: f64) -> (f64, f64, f64) {
        const DURATION_MS: f64 = 10_000.0;
        let engine_tick_ms = 1.0 + drift_ppm * 1e-6;
        let mut after_only = BridgeLog::new("sim", 15.0, Arc::default());
        let mut both = BridgeLog::new("sim", 15.0, Arc::default());
        let (mut level, mut area, mut now) = (15.0, 0.0, 0.0);
        let (mut next_engine, mut next_device) = (phase_ms, 0.0);
        while now < DURATION_MS {
            let next = f64::min(next_engine, next_device).min(DURATION_MS);
            area += level * (next - now);
            now = next;
            if now >= DURATION_MS {
                break;
            }
            if next_engine <= next_device {
                level += match side {
                    Side::Capture => -1.0,
                    Side::Render => 1.0,
                };
                next_engine += engine_tick_ms;
            } else {
                both.level(level);
                level += match side {
                    Side::Capture => period_ms,
                    Side::Render => -period_ms,
                };
                both.level(level);
                after_only.level(level);
                next_device += period_ms;
            }
        }
        (area / DURATION_MS, after_only.take_average(), both.take_average())
    }

    #[test]
    fn reading_only_after_each_packet_is_off_by_half_a_period() {
        for period_ms in [1.0, 3.0, 10.0, 20.0] {
            for phase_ms in [0.0, 0.25, 0.5, 0.75, 0.99] {
                for drift_ppm in [0.0, 100.0, -100.0] {
                    for (side, sign) in [(Side::Capture, 1.0), (Side::Render, -1.0)] {
                        let (truth, after_only, both) = simulate(side, period_ms, phase_ms, drift_ppm);
                        let case = format!("period {period_ms}, phase {phase_ms}, drift {drift_ppm}, sign {sign}");
                        let bias = after_only - truth;
                        assert!(
                            (bias - sign * period_ms / 2.0).abs() <= 0.5 + 1e-3,
                            "{case}: bias {bias}"
                        );
                        assert!((both - truth).abs() <= 0.5 + 1e-3, "{case}: error {}", both - truth);
                    }
                }
            }
        }
    }
    #[test]
    fn buffer_level_reads_back_the_published_pair() {
        let level = BufferLevel::default();
        assert_eq!(level.read(), None);
        level.publish(14.8, 15.5);
        assert_eq!(level.read(), Some((14.8, 15.5)));
        level.publish(-1.0, 15.0);
        assert_eq!(level.read(), Some((0.0, 15.0)));
        level.clear();
        assert_eq!(level.read(), None);
    }
}
