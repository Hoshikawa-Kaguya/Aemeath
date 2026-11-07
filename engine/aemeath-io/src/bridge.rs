//! 桥接线程共用的错误类型与统计日志。
//!
//! 日志是维护者长期验证的依据：每分钟记一次水位、漂移估计和校正量的摘要；溢出、欠载立即记录（限频）。

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

/// 桥接的统计与日志：每秒给控制器一个平均水位，每分钟记一次摘要；溢出、欠载立即记录（限频）。
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
    last_problem_log: Option<Instant>,
    last_update: RateUpdate,
}

/// 溢出、欠载的日志最多每隔这么久记一次
const PROBLEM_LOG_INTERVAL: Duration = Duration::from_secs(5);

impl BridgeLog {
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
            last_problem_log: None,
            last_update: RateUpdate::default(),
        }
    }

    pub fn level(&mut self, level_ms: f64) {
        self.level_sum += level_ms;
        self.level_count += 1;
        self.level_min = self.level_min.min(level_ms);
        self.level_max = self.level_max.max(level_ms);
    }

    pub fn control_due(&self) -> bool {
        self.last_control.elapsed() >= CONTROL_PERIOD && self.level_count > 0
    }

    pub fn take_average(&mut self) -> f64 {
        let average = self.level_sum / self.level_count.max(1) as f64;
        self.level_sum = 0.0;
        self.level_count = 0;
        self.last_control = Instant::now();
        average
    }

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
                "bridge status"
            );
            self.last_log = Instant::now();
            self.level_min = f64::MAX;
            self.level_max = 0.0;
        }
    }

    pub fn overflow(&mut self, frames: usize) {
        self.overflow_frames += frames as u64;
        self.problem("ring buffer overflow, frames dropped");
    }

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
            "bridge session ended"
        );
    }
}
