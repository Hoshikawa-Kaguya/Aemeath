//! 打开外部设备的 WASAPI 共享模式流，以及"打开 → 运行 → 出错后离线重试"的公共框架。
//!
//! 设备按 Windows 确定的设备格式（共享模式的混音格式，32 位浮点）收发，不使用 `AUTOCONVERTPCM`，
//! 采样率转换只在 Aemeath 内部完成（ADR-0010）。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use aemeath_protocol::ids::EndpointId;
use aemeath_protocol::message::{DeviceFormat, EndpointStatus, OfflineReason, SampleFormat};
use aemeath_protocol::units::SampleRate;
use wasapi::{AudioClient, DeviceEnumerator, Handle, SampleType, StreamMode, WasapiError, WaveFormat};

use crate::bridge::BridgeError;
use crate::{IoDirection, IoEvent, STEREO, StatusSink};

/// 设备被其他程序独占（S5 实测：Voicemeeter 的独占输出）
const AUDCLNT_E_DEVICE_IN_USE: u32 = 0x8889_000A;
/// 打不开时的重试间隔（选型报告 6.5 节的兜底重试；完整的离线检测在 M2）
const RETRY_INTERVAL: Duration = Duration::from_secs(2);
/// 等待停止信号时的检查间隔
const STOP_POLL: Duration = Duration::from_millis(100);
/// 等待设备事件的超时（毫秒）；超时后检查停止信号再继续等
pub const EVENT_WAIT_MS: u32 = 100;
/// 这么久没有设备事件时，主动查询一次设备，让失效（设备被拔出、重新连接）以错误的形式暴露出来
const PROBE_AFTER: Duration = Duration::from_secs(1);
/// 这么久没有设备事件、查询却又正常时，视为停滞：关闭这次会话，重新打开设备
const STALL_LIMIT: Duration = Duration::from_secs(5);
const BYTES_PER_F32: usize = 4;
const FLOAT_BITS: u16 = 32;

/// 已经初始化好的共享模式流。
pub struct OpenStream {
    pub client: AudioClient,
    pub event: Handle,
    pub channels: usize,
    pub block_align: usize,
    pub sample_rate: SampleRate,
}

impl OpenStream {
    pub fn device_format(&self) -> DeviceFormat {
        DeviceFormat {
            sample_rate: self.sample_rate,
            sample_format: SampleFormat::Float32,
            channels: u16::try_from(self.channels).unwrap_or(u16::MAX),
        }
    }

    /// 从交错的设备帧（32 位浮点）中取出立体声：单声道复制到左右两路，多声道只取前两路。
    pub fn append_stereo(&self, bytes: &[u8], out: &mut Vec<f32>) {
        for frame in bytes.chunks_exact(self.block_align) {
            for channel in 0..2 {
                let offset = channel.min(self.channels.saturating_sub(1)) * BYTES_PER_F32;
                let sample = frame
                    .get(offset..offset + BYTES_PER_F32)
                    .and_then(|bytes| bytes.try_into().ok())
                    .map(f32::from_le_bytes)
                    .unwrap_or(0.0);
                out.push(sample);
            }
        }
    }

    /// 把立体声帧写成设备帧：前两路写左右声道，其余声道补 0。
    pub fn write_stereo(&self, stereo: &[f32], out: &mut Vec<u8>) {
        out.clear();
        for frame in stereo.as_chunks::<STEREO>().0 {
            for channel in 0..self.channels {
                let sample = frame.get(channel).copied().unwrap_or(0.0);
                out.extend_from_slice(&sample.to_le_bytes());
            }
        }
    }
}

/// 一次等待的结果。
pub enum Waited {
    Event,
    /// 等待超时，但还不到停滞的程度：调用方检查停止信号后继续等
    Timeout,
}

/// 等待设备事件出错。
#[derive(Debug)]
pub enum WaitError {
    Device(WasapiError),
    /// 长时间没有事件，设备却没有报错
    Stalled,
}

/// 设备事件的看门狗。
///
/// 设备被拔出时，WASAPI 不一定再触发事件；只等事件的话，IO 线程可能一直卡住，既不报告离线也不恢复。
/// 所以超过 [`PROBE_AFTER`] 没有事件时查询一次设备，超过 [`STALL_LIMIT`] 时放弃这次会话。
pub struct EventWatch {
    last_event: Instant,
}

impl EventWatch {
    pub fn new() -> Self {
        Self {
            last_event: Instant::now(),
        }
    }

    pub fn wait(&mut self, stream: &OpenStream) -> Result<Waited, WaitError> {
        match stream.event.wait_for_event(EVENT_WAIT_MS) {
            Ok(()) => {
                self.last_event = Instant::now();
                Ok(Waited::Event)
            }
            Err(WasapiError::EventTimeout) => {
                let silent_for = self.last_event.elapsed();
                if silent_for >= PROBE_AFTER {
                    stream.client.get_current_padding().map_err(WaitError::Device)?;
                }
                if silent_for >= STALL_LIMIT {
                    Err(WaitError::Stalled)
                } else {
                    Ok(Waited::Timeout)
                }
            }
            Err(error) => Err(WaitError::Device(error)),
        }
    }
}

impl Default for EventWatch {
    fn default() -> Self {
        Self::new()
    }
}

/// 打开失败的原因。
pub enum OpenError {
    Offline(OfflineReason),
    Failed(WasapiError),
}

/// 把 WASAPI 错误归为离线原因。
pub fn offline_reason(error: &WasapiError) -> OfflineReason {
    match error {
        WasapiError::Windows(error) if error.code().0 as u32 == AUDCLNT_E_DEVICE_IN_USE => {
            OfflineReason::InUseByOtherApp
        }
        _ => OfflineReason::WaitingForDevice,
    }
}

pub fn open(endpoint: &EndpointId, direction: IoDirection) -> Result<OpenStream, OpenError> {
    let device = DeviceEnumerator::new()
        .and_then(|enumerator| enumerator.get_device(&endpoint.0))
        .map_err(|_| OpenError::Offline(OfflineReason::WaitingForDevice))?;
    let mut client = device
        .get_iaudioclient()
        .map_err(|error| OpenError::Offline(offline_reason(&error)))?;
    let format = client.get_mixformat().map_err(OpenError::Failed)?;
    check_float(&format).map_err(OpenError::Failed)?;
    let (default_period, _) = client.get_device_period().map_err(OpenError::Failed)?;
    let wasapi_direction = match direction {
        IoDirection::Input => wasapi::Direction::Capture,
        IoDirection::Output => wasapi::Direction::Render,
    };
    client
        .initialize_client(
            &format,
            wasapi_direction,
            StreamMode::EventsShared {
                autoconvert: false,
                buffer_duration_hns: default_period,
            },
            None,
        )
        .map_err(|error| OpenError::Offline(offline_reason(&error)))?;
    let event = client.set_get_eventhandle().map_err(OpenError::Failed)?;
    let sample_rate =
        SampleRate::new(format.get_samplespersec()).map_err(|_| OpenError::Failed(WasapiError::UnsupportedFormat))?;
    Ok(OpenStream {
        client,
        event,
        channels: usize::from(format.get_nchannels()),
        block_align: format.get_blockalign() as usize,
        sample_rate,
    })
}

/// 共享模式的混音格式在 Windows 上总是 32 位浮点；不是时视为不支持。
fn check_float(format: &WaveFormat) -> Result<(), WasapiError> {
    match format.get_subformat()? {
        SampleType::Float if format.get_bitspersample() == FLOAT_BITS => Ok(()),
        _ => Err(WasapiError::UnsupportedFormat),
    }
}

/// 一次会话（一次成功打开设备之后）结束的原因。
pub enum SessionEnd {
    /// 收到停止信号，或者引擎一侧已经放弃了这条连接：线程应退出
    Finished,
    /// 设备或重采样出错（例如设备被拔出）：报告离线，稍后重试
    Failed { reason: OfflineReason, message: String },
}

impl SessionEnd {
    pub fn failed(error: &BridgeError) -> Self {
        Self::Failed {
            reason: error.offline_reason(),
            message: error.to_string(),
        }
    }
}

/// IO 线程的主循环：反复打开设备并运行会话，出错时报告离线状态，每隔一段时间重试。
pub fn run_with_retry(
    endpoint: &EndpointId,
    direction: IoDirection,
    stop: &AtomicBool,
    status: &StatusSink,
    mut session: impl FnMut(&OpenStream) -> SessionEnd,
) {
    if wasapi::initialize_mta().is_err() {
        tracing::error!(endpoint = %endpoint.0, "cannot initialize COM for the device thread");
        return;
    }
    let report = |state: EndpointStatus| {
        status(IoEvent {
            endpoint: endpoint.clone(),
            direction,
            status: state,
        });
    };
    let mut last_offline: Option<OfflineReason> = None;
    while !stop.load(Ordering::Relaxed) {
        match open(endpoint, direction) {
            Ok(stream) => {
                tracing::info!(endpoint = %endpoint.0, ?direction, format = ?stream.device_format(), "device opened");
                report(EndpointStatus::Online {
                    format: stream.device_format(),
                });
                match session(&stream) {
                    SessionEnd::Finished => return,
                    SessionEnd::Failed { reason, message } => {
                        tracing::warn!(endpoint = %endpoint.0, ?direction, %message, "device lost");
                        report(EndpointStatus::Offline { reason });
                        last_offline = Some(reason);
                    }
                }
            }
            Err(error) => {
                let reason = match &error {
                    OpenError::Offline(reason) => *reason,
                    OpenError::Failed(_) => OfflineReason::WaitingForDevice,
                };
                if last_offline != Some(reason) {
                    if let OpenError::Failed(error) = &error {
                        tracing::warn!(endpoint = %endpoint.0, ?direction, %error, "cannot open device");
                    } else {
                        tracing::info!(endpoint = %endpoint.0, ?direction, ?reason, "device offline");
                    }
                    report(EndpointStatus::Offline { reason });
                    last_offline = Some(reason);
                }
            }
        }
        sleep_unless_stopped(RETRY_INTERVAL, stop);
    }
}

fn sleep_unless_stopped(duration: Duration, stop: &AtomicBool) {
    let mut remaining = duration;
    while !remaining.is_zero() && !stop.load(Ordering::Relaxed) {
        let step = remaining.min(STOP_POLL);
        thread::sleep(step);
        remaining -= step;
    }
}

/// 线程间共享的停止信号。
pub fn stop_flag() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}
