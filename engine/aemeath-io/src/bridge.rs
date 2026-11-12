//! 桥接线程共用的错误类型与统计日志。
//!
//! 长时间运行出问题时要靠日志排查：每分钟记一次缓冲占用、漂移估计和校正量的摘要；溢出、欠载立即记录（限频）。

use std::time::{Duration, Instant};

use aemeath_dsp::resample::{RateUpdate, ResampleError};
use aemeath_protocol::message::OfflineReason;
use wasapi::WasapiError;

use crate::device;

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
}

/// 溢出、欠载的日志最多每隔这么久记一次
const PROBLEM_LOG_INTERVAL: Duration = Duration::from_secs(5);

impl BridgeLog {
    /// 为一个端点开始一段新的统计；`endpoint` 只用于日志里区分设备。
    pub fn new(endpoint: &str) -> Self {
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
