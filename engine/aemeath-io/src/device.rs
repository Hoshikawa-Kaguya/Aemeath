//! 打开外部设备的 WASAPI 流，以及"打开 → 运行 → 出错后离线重试"的公共框架。
//!
//! 打开方式按节点的设备设置（ADR-0014）：
//! - 独占（默认）：格式取用户指定的或推荐的（[`DeviceCapabilities::recommended`]），周期按缓冲设置并对齐到设备要求。
//!   打不开时不退回共享模式，报告离线原因。
//! - 共享：按 Windows 的混音格式（32 位浮点）收发。
//!
//! 两种方式都不使用 `AUTOCONVERTPCM`，采样率转换只在 Aemeath 内部完成（ADR-0010）。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use aemeath_dsp::codec;
use aemeath_protocol::ids::EndpointId;
use aemeath_protocol::message::{DeviceCapabilities, DeviceFormat, EndpointStatus, OfflineReason, SampleFormat};
use aemeath_protocol::topology::{BufferChoice, DeviceAccess, FormatChoice};
use aemeath_protocol::units::SampleRate;
use wasapi::{AudioClient, DeviceEnumerator, Handle, SampleType, StreamMode, WasapiError, WaveFormat};

use crate::bridge::BridgeError;
use crate::capabilities::{CapabilityCache, microseconds, quick_recommended};
use crate::format::{to_device_format, to_wave_format};
use crate::redetect::{Redetect, retry_interval};
use crate::{IoDirection, IoEvent, STEREO, StatusSink};

/// 其他程序占着设备（S5 实测：Voicemeeter 的独占输出；独占打开时，其他程序共享使用也会这样）
const AUDCLNT_E_DEVICE_IN_USE: u32 = 0x8889_000A;
/// Windows 声音设置里关掉了"允许应用程序独占控制"
const AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED: u32 = 0x8889_000E;
/// 独占模式的周期没有对齐到设备要求：用 `GetBufferSize` 得到的对齐值重建客户端再初始化一次
const AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED: u32 = 0x8889_0019;
/// 独占时设备不接受这个格式
const AUDCLNT_E_UNSUPPORTED_FORMAT: u32 = 0x8889_0008;
/// 等待设备事件的超时（毫秒）；超时后检查停止信号再继续等
pub const EVENT_WAIT_MS: u32 = 100;
/// 这么久没有设备事件时，主动查询一次设备，让失效（设备被拔出、重新连接）以错误的形式暴露出来
const PROBE_AFTER: Duration = Duration::from_secs(1);
/// 这么久没有设备事件、查询却又正常时，判定为"无信号"（选型报告 6.5 节）：关闭这次会话，重新打开设备
const NO_SIGNAL_AFTER: Duration = Duration::from_secs(3);
const HNS_PER_MILLISECOND: f64 = 10_000.0;
const HNS_PER_SECOND: f64 = 10_000_000.0;
const FLOAT_BITS: u16 = 32;

/// 已经初始化好的流。
pub struct OpenStream {
    pub client: AudioClient,
    pub event: Handle,
    /// 实际使用的设备格式
    pub format: DeviceFormat,
    pub block_align: usize,
    pub exclusive: bool,
}

impl OpenStream {
    pub fn sample_rate(&self) -> SampleRate {
        self.format.sample_rate
    }

    pub fn channels(&self) -> usize {
        usize::from(self.format.channels)
    }

    /// 这条流的设备帧与立体声 f32 之间的转换器（每个会话一个，内部缓冲复用，不在循环里分配）。
    pub fn frame_codec(&self) -> FrameCodec {
        FrameCodec {
            sample_format: self.format.sample_format,
            channels: self.channels(),
            block_align: self.block_align,
            samples: Vec::new(),
        }
    }
}

/// 设备帧（任意样本格式和声道数）与立体声 f32 之间的转换：单声道复制到左右两路，多声道只取前两路、写出时其余补 0。
pub struct FrameCodec {
    sample_format: SampleFormat,
    channels: usize,
    block_align: usize,
    samples: Vec<f32>,
}

impl FrameCodec {
    /// 把交错的设备帧解码，追加立体声到 `out`。
    pub fn append_stereo(&mut self, bytes: &[u8], out: &mut Vec<f32>) {
        let frames = bytes.len() / self.block_align.max(1);
        self.samples.clear();
        codec::decode(
            self.sample_format,
            &bytes[..frames * self.block_align],
            &mut self.samples,
        );
        for frame in self.samples.chunks_exact(self.channels.max(1)) {
            let left = frame.first().copied().unwrap_or(0.0);
            out.push(left);
            out.push(frame.get(1).copied().unwrap_or(left));
        }
    }

    /// 把立体声编码成设备帧，写进 `out`（先清空）。
    pub fn write_stereo(&mut self, stereo: &[f32], out: &mut Vec<u8>) {
        self.samples.clear();
        for frame in stereo.as_chunks::<STEREO>().0 {
            for channel in 0..self.channels {
                self.samples.push(frame.get(channel).copied().unwrap_or(0.0));
            }
        }
        out.clear();
        codec::encode(self.sample_format, &self.samples, out);
    }
}

/// 一次等待的结果。
pub enum Waited {
    Event,
    /// 等待超时，但还不到无信号的程度：调用方检查停止信号后继续等
    Timeout,
}

/// 等待设备事件出错。
#[derive(Debug)]
pub enum WaitError {
    Device(WasapiError),
    /// 长时间没有事件，设备却没有报错：无信号
    Stalled,
}

/// 设备事件的看门狗。
///
/// 设备被拔出时，WASAPI 不一定再触发事件；只等事件的话，IO 线程可能一直卡住，既不报告离线也不恢复。
/// 所以超过 [`PROBE_AFTER`] 没有事件时查询一次设备，超过 [`NO_SIGNAL_AFTER`] 时放弃这次会话。
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
                if silent_for >= NO_SIGNAL_AFTER {
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
    /// 独占时设备不接受所选的格式
    Unsupported,
}

fn hresult(error: &WasapiError) -> Option<u32> {
    match error {
        WasapiError::Windows(error) => Some(error.code().0 as u32),
        _ => None,
    }
}

/// 把 WASAPI 错误归为离线原因。
pub fn offline_reason(error: &WasapiError) -> OfflineReason {
    match hresult(error) {
        Some(AUDCLNT_E_DEVICE_IN_USE) => OfflineReason::InUseByOtherApp,
        Some(AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED) => OfflineReason::ExclusiveNotAllowed,
        _ => OfflineReason::WaitingForDevice,
    }
}

/// 打开一台外部设备所需的全部信息。
#[derive(Debug, Clone)]
pub struct DeviceRequest {
    pub endpoint: EndpointId,
    pub direction: IoDirection,
    pub access: DeviceAccess,
    pub engine_rate: SampleRate,
}

fn wasapi_direction(direction: IoDirection) -> wasapi::Direction {
    match direction {
        IoDirection::Input => wasapi::Direction::Capture,
        IoDirection::Output => wasapi::Direction::Render,
    }
}

/// 打开设备，并把当前的格式和周期登记到 `cache`，供主程序查询（ADR-0014）。
/// 不做全量探测：探测会切换 USB 设备的格式（见 [`crate::capabilities`]）。
pub fn open(request: &DeviceRequest, cache: &CapabilityCache) -> Result<OpenStream, OpenError> {
    let device = DeviceEnumerator::new()
        .and_then(|enumerator| enumerator.get_device(&request.endpoint.0))
        .map_err(|_| OpenError::Offline(OfflineReason::WaitingForDevice))?;
    let stream = match request.access {
        DeviceAccess::Shared => open_shared(&device, request.direction)?,
        DeviceAccess::Exclusive { format, buffer } => {
            match open_exclusive(&device, request, cache, format, buffer) {
                // 用户指定的格式设备不再接受（例如换了一台同名设备）：退回推荐格式，并记录警告
                Err(OpenError::Unsupported) if format != FormatChoice::Recommended => {
                    tracing::warn!(endpoint = %request.endpoint.0, ?format,
                        "the chosen device format is not supported, using the recommended one");
                    open_exclusive(&device, request, cache, FormatChoice::Recommended, buffer)?
                }
                Err(OpenError::Unsupported) => return Err(OpenError::Offline(OfflineReason::WaitingForDevice)),
                result => result?,
            }
        }
    };
    let (default_period, minimum_period) = stream.client.get_device_period().unwrap_or_default();
    let shared_format = stream
        .client
        .get_mixformat()
        .ok()
        .and_then(|mix| to_device_format(&mix));
    cache.opened(
        &request.endpoint,
        request.direction,
        DeviceCapabilities {
            exclusive_formats: if stream.exclusive {
                vec![stream.format]
            } else {
                Vec::new()
            },
            shared_format,
            default_period_us: microseconds(default_period),
            minimum_period_us: microseconds(minimum_period),
        },
    );
    Ok(stream)
}

fn open_shared(device: &wasapi::Device, direction: IoDirection) -> Result<OpenStream, OpenError> {
    let mut client = device
        .get_iaudioclient()
        .map_err(|error| OpenError::Offline(offline_reason(&error)))?;
    let wave = client.get_mixformat().map_err(OpenError::Failed)?;
    // 共享模式的混音格式在 Windows 上总是 32 位浮点；不是时视为不支持
    let is_float = matches!(wave.get_subformat(), Ok(SampleType::Float)) && wave.get_bitspersample() == FLOAT_BITS;
    if !is_float {
        return Err(OpenError::Failed(WasapiError::UnsupportedFormat));
    }
    let (default_period, _) = client.get_device_period().map_err(OpenError::Failed)?;
    client
        .initialize_client(
            &wave,
            wasapi_direction(direction),
            StreamMode::EventsShared {
                autoconvert: false,
                buffer_duration_hns: default_period,
            },
            None,
        )
        .map_err(|error| OpenError::Offline(offline_reason(&error)))?;
    finish(client, &wave, false)
}

fn open_exclusive(
    device: &wasapi::Device,
    request: &DeviceRequest,
    cache: &CapabilityCache,
    format: FormatChoice,
    buffer: BufferChoice,
) -> Result<OpenStream, OpenError> {
    let mut client = device
        .get_iaudioclient()
        .map_err(|error| OpenError::Offline(offline_reason(&error)))?;
    let shared = client.get_mixformat().ok().and_then(|mix| to_device_format(&mix));
    let chosen = match format {
        FormatChoice::Fixed { format } => Some(format),
        // 推荐格式：有全量探测的缓存就用它，否则只试少数几个候选
        FormatChoice::Recommended => cache
            .probed(&request.endpoint, request.direction)
            .and_then(|probed| probed.recommended(request.engine_rate))
            .or_else(|| quick_recommended(&client, request.engine_rate, shared)),
    };
    // 一个独占格式也试不出来（例如设备正被其他程序占着）：仍用共享格式试一次，让 Initialize 给出真正的原因
    let Some(chosen) = chosen.or(shared) else {
        return Err(OpenError::Offline(OfflineReason::WaitingForDevice));
    };
    let requested = to_wave_format(chosen);
    // 有的驱动只接受特定的声道掩码或 WAVEFORMATEX；找不到可接受的写法时按原样初始化，由 Initialize 报告错误
    let wave = client
        .is_supported_exclusive_with_quirks(&requested)
        .unwrap_or(requested);
    let (default_period, minimum_period) = client.get_device_period().map_err(OpenError::Failed)?;
    let desired = match buffer {
        BufferChoice::DeviceDefault => default_period,
        BufferChoice::DeviceMinimum => minimum_period,
        BufferChoice::Milliseconds { ms } => (f64::from(ms.ms()) * HNS_PER_MILLISECOND).round() as i64,
    };
    let period = client
        .calculate_aligned_period_near(desired, None, &wave)
        .map_err(OpenError::Failed)?;
    let direction = wasapi_direction(request.direction);
    let mode = StreamMode::EventsExclusive { period_hns: period };
    match client.initialize_client(&wave, direction, mode, None) {
        Ok(()) => {}
        Err(error) if hresult(&error) == Some(AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED) => {
            let frames = client.get_buffer_size().map_err(OpenError::Failed)?;
            let aligned = (f64::from(frames) * HNS_PER_SECOND / f64::from(wave.get_samplespersec())).round() as i64;
            tracing::info!(endpoint = %request.endpoint.0, period, aligned, "realigning the exclusive period");
            client = device
                .get_iaudioclient()
                .map_err(|error| OpenError::Offline(offline_reason(&error)))?;
            client
                .initialize_client(
                    &wave,
                    direction,
                    StreamMode::EventsExclusive { period_hns: aligned },
                    None,
                )
                .map_err(|error| OpenError::Offline(offline_reason(&error)))?;
        }
        Err(error) if hresult(&error) == Some(AUDCLNT_E_UNSUPPORTED_FORMAT) => return Err(OpenError::Unsupported),
        Err(error) => return Err(OpenError::Offline(offline_reason(&error))),
    }
    finish(client, &wave, true)
}

fn finish(client: AudioClient, wave: &WaveFormat, exclusive: bool) -> Result<OpenStream, OpenError> {
    let event = client.set_get_eventhandle().map_err(OpenError::Failed)?;
    let format = to_device_format(wave).ok_or(OpenError::Failed(WasapiError::UnsupportedFormat))?;
    Ok(OpenStream {
        client,
        event,
        format,
        block_align: wave.get_blockalign() as usize,
        exclusive,
    })
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

/// IO 线程共用的环境。
#[derive(Clone)]
pub struct IoContext {
    pub status: StatusSink,
    pub capabilities: CapabilityCache,
    pub redetect: Redetect,
}

/// IO 线程的主循环：反复打开设备并运行会话，出错时报告离线状态，按选型报告 6.5 节的节奏重试。
pub fn run_with_retry(
    request: &DeviceRequest,
    stop: &AtomicBool,
    context: &IoContext,
    mut session: impl FnMut(&OpenStream) -> SessionEnd,
) {
    if wasapi::initialize_mta().is_err() {
        tracing::error!(endpoint = %request.endpoint.0, "cannot initialize COM for the device thread");
        return;
    }
    let endpoint = &request.endpoint;
    let direction = request.direction;
    let report = |state: EndpointStatus| {
        (context.status)(IoEvent {
            endpoint: endpoint.clone(),
            direction,
            status: state,
        });
    };
    let mut last_offline: Option<OfflineReason> = None;
    let mut offline_since = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        let reason = match open(request, &context.capabilities) {
            Ok(stream) => {
                tracing::info!(
                    endpoint = %endpoint.0,
                    ?direction,
                    format = ?stream.format,
                    exclusive = stream.exclusive,
                    "device opened"
                );
                report(EndpointStatus::Online { format: stream.format });
                let end = session(&stream);
                context.capabilities.closed(endpoint, direction);
                match end {
                    SessionEnd::Finished => return,
                    SessionEnd::Failed { reason, message } => {
                        tracing::warn!(endpoint = %endpoint.0, ?direction, %message, "device lost");
                        report(EndpointStatus::Offline { reason });
                        last_offline = Some(reason);
                        offline_since = Instant::now();
                        reason
                    }
                }
            }
            Err(error) => {
                let reason = match &error {
                    OpenError::Offline(reason) => *reason,
                    OpenError::Failed(_) | OpenError::Unsupported => OfflineReason::WaitingForDevice,
                };
                if last_offline != Some(reason) {
                    if let OpenError::Failed(error) = &error {
                        tracing::warn!(endpoint = %endpoint.0, ?direction, %error, "cannot open device");
                    } else {
                        tracing::info!(endpoint = %endpoint.0, ?direction, ?reason, "device offline");
                    }
                    if last_offline.is_none() {
                        offline_since = Instant::now();
                    }
                    report(EndpointStatus::Offline { reason });
                    last_offline = Some(reason);
                }
                reason
            }
        };
        // "设备不允许独占"重试无用：只等端点变化通知或用户的"重新检测"
        let timeout = (reason != OfflineReason::ExclusiveNotAllowed).then(|| retry_interval(offline_since.elapsed()));
        context.redetect.wait(endpoint, timeout, stop);
    }
}

/// 线程间共享的停止信号。
pub fn stop_flag() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codec(sample_format: SampleFormat, channels: usize) -> FrameCodec {
        FrameCodec {
            sample_format,
            channels,
            block_align: channels * codec::bytes_per_sample(sample_format),
            samples: Vec::new(),
        }
    }

    #[test]
    fn mono_is_copied_to_both_channels() {
        let mut mono = codec(SampleFormat::Float32, 1);
        let mut bytes = Vec::new();
        codec::encode(SampleFormat::Float32, &[0.5, -0.25], &mut bytes);
        let mut stereo = Vec::new();
        mono.append_stereo(&bytes, &mut stereo);
        assert_eq!(stereo, vec![0.5, 0.5, -0.25, -0.25]);
    }

    #[test]
    fn extra_channels_are_dropped_and_padded() {
        let mut surround = codec(SampleFormat::Int16, 4);
        let mut bytes = Vec::new();
        surround.write_stereo(&[0.5, -0.5], &mut bytes);
        let mut decoded = Vec::new();
        codec::decode(SampleFormat::Int16, &bytes, &mut decoded);
        assert_eq!(decoded, vec![0.5, -0.5, 0.0, 0.0]);
        let mut stereo = Vec::new();
        surround.append_stereo(&bytes, &mut stereo);
        assert_eq!(stereo, vec![0.5, -0.5]);
    }
}
