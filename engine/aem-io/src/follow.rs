//! 主时钟跟随（S4）：让所有虚拟设备的时间线锁定到主时钟设备上。
//!
//! 以静音方式另开一条主时钟设备的共享流（播放端写静音，录音端读出丢弃），每秒读取一次 `IAudioClock`
//! 的位置与对应的 QPC：
//! - 频率：对最近 30 个样本做"主时钟秒数 ~ QPC 秒数"的线性回归，斜率就是主时钟相对 QPC 的速率；
//! - 相位：时间线实际走过的微帧数与按主时钟应走过的微帧数之差，以 [`KP_PER_SECOND`] 的速度拉回（锁相环）。
//!   只锁频率时残余误差会累积成相位漂移；锁相位后虚拟设备不需要 ASRC（S4 实测相位误差 ±18 µs）。
//!
//! 时间线通过 [`ClockTarget`] 调整，所以本 crate 不依赖引擎。线程退出时把它交还调用方，供下一个跟随线程使用。

use std::collections::VecDeque;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use aem_protocol::ids::EndpointId;
use wasapi::{AudioClient, WasapiError};

use crate::bridge::BridgeError;
use crate::device::{self, EventWatch, OpenStream, SessionEnd, Waited};
use crate::{IoDirection, STEREO, StatusSink};

const MICROFRAMES_PER_SECOND: f64 = 8_000.0;
/// 回归窗口：最近 30 个样本（30 s）
const WINDOW: usize = 30;
/// 至少这么多个样本后才开始调整
const MIN_SAMPLES: usize = 5;
/// 相位误差的回拉速度（1/s）
pub const KP_PER_SECOND: f64 = 0.1;
/// 相位校正量上限（相对速率）
const MAX_PHASE_CORRECTION: f64 = 100e-6;
const SAMPLE_PERIOD: Duration = Duration::from_secs(1);
/// 开流后跳过这段再开始采样，避开启动时的抖动
const WARM_UP: Duration = Duration::from_secs(2);
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
    pub fn spawn(endpoint: EndpointId, direction: IoDirection, target: Box<dyn ClockTarget>) -> io::Result<Self> {
        let stop = device::stop_flag();
        let thread_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("aem-clock-follow".to_owned())
            .spawn(move || run(&endpoint, direction, target, &thread_stop))?;
        Ok(Self { stop, thread })
    }

    /// 结束跟随并交还时间线；线程异常退出时返回 None。
    pub fn stop(self) -> Option<Box<dyn ClockTarget>> {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.join().ok()
    }
}

fn run(
    endpoint: &EndpointId,
    direction: IoDirection,
    mut target: Box<dyn ClockTarget>,
    stop: &AtomicBool,
) -> Box<dyn ClockTarget> {
    // 跟随线程只读取时钟，不参与实时音频，不提升优先级。
    // 主时钟设备的状态由它自己的桥接线程报告，这里不重复报告
    let quiet: StatusSink = Arc::new(|_| {});
    device::run_with_retry(endpoint, direction, stop, &quiet, |stream| {
        match follow(stream, direction, target.as_mut(), stop) {
            Ok(()) => SessionEnd::Finished,
            Err(error) => SessionEnd::failed(&error),
        }
    });
    target
}

fn follow(
    stream: &OpenStream,
    direction: IoDirection,
    target: &mut dyn ClockTarget,
    stop: &AtomicBool,
) -> Result<(), BridgeError> {
    let clock = stream.client.get_audioclock()?;
    let frequency = clock.get_frequency()? as f64;
    let mut io = SilentIo::new(stream, direction)?;
    stream.client.start_stream()?;
    let mut watch = EventWatch::new();
    tracing::info!(?direction, "clock master following started");

    let started = Instant::now();
    let mut next_sample = started + WARM_UP;
    let mut last_log = started;
    // (主时钟位置, QPC, 锁定时的时间线位置)
    let mut origin: Option<(u64, i64, f64)> = None;
    let mut window: VecDeque<(f64, f64)> = VecDeque::with_capacity(WINDOW + 1);
    while !stop.load(Ordering::Relaxed) {
        if let Waited::Event = watch.wait(stream)? {
            io.service(stream)?;
        }
        if Instant::now() < next_sample {
            continue;
        }
        next_sample += SAMPLE_PERIOD;

        let (position, qpc) = clock.get_position()?;
        let qpc = i64::try_from(qpc).unwrap_or(i64::MAX);
        let (origin_position, origin_qpc, origin_microframe) =
            *origin.get_or_insert((position, qpc, target.position(qpc)));
        let master_seconds = position.saturating_sub(origin_position) as f64 / frequency;
        let real_seconds = (qpc - origin_qpc) as f64 / HNS_PER_SECOND;
        window.push_back((real_seconds, master_seconds));
        if window.len() > WINDOW {
            window.pop_front();
        }
        if window.len() < MIN_SAMPLES {
            continue;
        }

        let rate = slope(&window);
        let desired = origin_microframe + master_seconds * MICROFRAMES_PER_SECOND;
        let phase_error_seconds = (target.position(qpc) - desired) / MICROFRAMES_PER_SECOND;
        let correction = (-KP_PER_SECOND * phase_error_seconds).clamp(-MAX_PHASE_CORRECTION, MAX_PHASE_CORRECTION);
        let factor = rate * (1.0 + correction);
        target.set_rate(factor, aem_sys::clock::now_hns());

        if last_log.elapsed() >= LOG_PERIOD {
            tracing::info!(
                master_ppm = format!("{:.2}", (rate - 1.0) * PPM),
                phase_error_us = format!("{:.1}", phase_error_seconds * PPM),
                applied_ppm = format!("{:.2}", (factor - 1.0) * PPM),
                "clock master following"
            );
            last_log = Instant::now();
        }
    }
    let _ = stream.client.stop_stream();
    Ok(())
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

/// 让跟随用的流保持运转：播放端持续写静音，录音端读出丢弃。
enum SilentIo {
    Render {
        client: wasapi::AudioRenderClient,
        silence: Vec<u8>,
    },
    Capture {
        client: wasapi::AudioCaptureClient,
        scratch: Vec<u8>,
    },
}

impl SilentIo {
    fn new(stream: &OpenStream, direction: IoDirection) -> Result<Self, WasapiError> {
        match direction {
            IoDirection::Output => {
                let client = stream.client.get_audiorenderclient()?;
                let silence = Vec::new();
                let mut io = Self::Render { client, silence };
                io.service(stream)?;
                Ok(io)
            }
            IoDirection::Input => Ok(Self::Capture {
                client: stream.client.get_audiocaptureclient()?,
                scratch: Vec::new(),
            }),
        }
    }

    fn service(&mut self, stream: &OpenStream) -> Result<(), WasapiError> {
        match self {
            Self::Render { client, silence } => {
                let frames = available(&stream.client)?;
                stream.write_stereo(&vec![0.0; frames * STEREO], silence);
                client.write_to_device(silence, None)
            }
            Self::Capture { client, scratch } => {
                while let Some(frames) = client.get_next_packet_size()?.filter(|frames| *frames > 0) {
                    scratch.resize(frames as usize * stream.block_align, 0);
                    client.read_from_device(scratch)?;
                }
                Ok(())
            }
        }
    }
}

fn available(client: &AudioClient) -> Result<usize, WasapiError> {
    Ok(client.get_available_space_in_frames()? as usize)
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
}
