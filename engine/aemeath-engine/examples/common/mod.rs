//! 端到端示例共用的部分：挂载开发槽位、查找设备、播放正弦波、录音并逐样本检查正弦的连续性。
//!
//! 连续性用正弦递推 x[n] = 2cos(ω)·x[n-1] − x[n-2] 检查：任何丢帧、重复或跳变都会让残差突增。

#![allow(dead_code)]

use std::error::Error;
use std::f64::consts::TAU;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use aemeath_protocol::ids::{EndpointId, SlotDirection, SlotId, SlotNumber};
use wasapi::{DeviceEnumerator, SampleType, StreamMode};

pub type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

pub const USBIP: &str = r"C:\Program Files\USBip\usbip.exe";
/// 避开默认端口上残留的后台重连
pub const PORT: u16 = 32411;
pub const SINE_HZ: f64 = 1_000.0;
/// 残差超过"幅度 × 这个比例"算一次中断（f32 直通时残差约 1e-6，重采样后约 1e-5 量级）
const GLITCH_RATIO: f64 = 0.04;
/// 一次中断之后这么多个样本内的残差不重复计数
const GLITCH_HOLDOFF: u64 = 64;
/// 信号出现后先跳过这段再开始检查
const SETTLE_SECONDS: f64 = 0.5;
/// 超过"幅度 × 这个比例"视为信号已出现
const SIGNAL_RATIO: f64 = 0.1;
const DEVICE_TIMEOUT: Duration = Duration::from_secs(60);
const EVENT_TIMEOUT_MS: u32 = 2_000;
const FLOAT_BITS: u16 = 32;
const POLL_INTERVAL: Duration = Duration::from_millis(5);

pub fn slot(direction: SlotDirection, number: u8) -> Result<SlotId> {
    Ok(SlotId {
        direction,
        number: SlotNumber::new(number)?,
    })
}

pub fn attach(port: u16, busid: &str) {
    let port = port.to_string();
    let output = Command::new(USBIP)
        .args(["-t", &port, "attach", "-r", "127.0.0.1", "-b", busid, "--once"])
        .output();
    match output {
        Ok(output) => println!("attach {busid}: {}", String::from_utf8_lossy(&output.stdout).trim()),
        Err(error) => println!("attach {busid}: cannot run usbip.exe: {error}"),
    }
}

/// 挂载开发槽位；离开作用域时（包括出错提前返回）卸下所有指向这个端口的挂载。
pub struct Attachment {
    port: u16,
}

impl Attachment {
    pub fn attach(port: u16, bus_ids: &[&str]) -> Self {
        for busid in bus_ids {
            attach(port, busid);
        }
        Self { port }
    }
}

impl Drop for Attachment {
    fn drop(&mut self) {
        detach_all(self.port);
    }
}

/// 等待线程结束；出错时在错误中写明是哪个线程，以及出错的时间。
pub fn join<T>(name: &str, handle: thread::JoinHandle<Result<T>>) -> Result<T> {
    match handle.join() {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(format!("{name} failed: {error}").into()),
        Err(_) => Err(format!("{name} panicked").into()),
    }
}

/// 给线程的错误标上发生时刻（相对 `started`）。
pub fn timed<T>(started: Instant, result: Result<T>) -> Result<T> {
    result.map_err(|error| format!("{error} (after {:.1} s)", started.elapsed().as_secs_f64()).into())
}

/// 卸下所有指向这个端口的挂载。
pub fn detach_all(port: u16) {
    let Ok(output) = Command::new(USBIP).arg("port").output() else {
        return;
    };
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    let needle = format!("127.0.0.1:{port}/");
    let mut current: Option<String> = None;
    for line in text.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("Port ") {
            current = rest
                .split(':')
                .next()
                .map(|port| port.trim().trim_start_matches('0').to_owned());
        } else if line.contains(&needle)
            && let Some(port) = current.take()
        {
            let port = if port.is_empty() { "0".to_owned() } else { port };
            let _ = Command::new(USBIP).args(["detach", "-p", &port]).output();
            println!("detached port {port}");
        }
    }
}

/// 按名称片段查找设备（等待它出现）；名称为 None 时用系统默认设备。
pub fn find_device(direction: wasapi::Direction, name: Option<&str>) -> Result<wasapi::Device> {
    let enumerator = DeviceEnumerator::new()?;
    let Some(name) = name else {
        return Ok(enumerator.get_default_device(direction)?);
    };
    let deadline = Instant::now() + DEVICE_TIMEOUT;
    loop {
        for device in &DeviceEnumerator::new()?.get_device_collection(direction)? {
            let device = device?;
            if device.get_friendlyname().is_ok_and(|friendly| friendly.contains(name)) {
                return Ok(device);
            }
        }
        if Instant::now() > deadline {
            return Err(format!("device {name} did not appear").into());
        }
        thread::sleep(Duration::from_millis(500));
    }
}

pub fn endpoint_id(device: &wasapi::Device) -> Result<EndpointId> {
    Ok(EndpointId(device.get_id()?))
}

fn require_float32(format: &wasapi::WaveFormat) -> Result<()> {
    if matches!(format.get_subformat()?, SampleType::Float) && format.get_bitspersample() == FLOAT_BITS {
        Ok(())
    } else {
        Err("the shared-mode mix format is not 32-bit float".into())
    }
}

/// 像普通应用一样往设备播放 1 kHz 正弦波（共享模式）。返回实际播放的秒数。
pub fn render_sine(name: &str, seconds: f64, amplitude: f64) -> Result<f64> {
    wasapi::initialize_mta().ok()?;
    let device = find_device(wasapi::Direction::Render, Some(name))?;
    let mut client = device.get_iaudioclient()?;
    let format = client.get_mixformat()?;
    require_float32(&format)?;
    let (default_period, _) = client.get_device_period()?;
    client.initialize_client(
        &format,
        wasapi::Direction::Render,
        StreamMode::EventsShared {
            autoconvert: false,
            buffer_duration_hns: default_period,
        },
        None,
    )?;
    let event = client.set_get_eventhandle()?;
    let render_client = client.get_audiorenderclient()?;
    let channels = usize::from(format.get_nchannels());
    let rate = f64::from(format.get_samplespersec());
    let total_frames = (seconds * rate) as u64;
    let mut written = 0u64;
    let mut bytes = Vec::new();
    let fill = |frames: usize, written: &mut u64, bytes: &mut Vec<u8>| {
        bytes.clear();
        for _ in 0..frames {
            let sample = if *written < total_frames {
                (amplitude * (TAU * SINE_HZ * *written as f64 / rate).sin()) as f32
            } else {
                0.0
            };
            for _ in 0..channels {
                bytes.extend_from_slice(&sample.to_le_bytes());
            }
            *written += 1;
        }
    };

    fill(
        client.get_available_space_in_frames()? as usize,
        &mut written,
        &mut bytes,
    );
    render_client.write_to_device(&bytes, None)?;
    client.start_stream()?;
    while written < total_frames {
        event.wait_for_event(EVENT_TIMEOUT_MS)?;
        fill(
            client.get_available_space_in_frames()? as usize,
            &mut written,
            &mut bytes,
        );
        render_client.write_to_device(&bytes, None)?;
    }
    client.stop_stream()?;
    Ok(written as f64 / rate)
}

#[derive(Debug, Default)]
pub struct Analysis {
    pub sample_rate: u32,
    /// 检查过的样本数
    pub checked: u64,
    pub peak: f64,
    pub glitches: u64,
    pub discontinuities: u64,
}

impl Analysis {
    pub fn checked_seconds(&self) -> f64 {
        self.checked as f64 / f64::from(self.sample_rate.max(1))
    }
}

/// 录音 `seconds` 秒，检查幅度约为 `amplitude` 的 1 kHz 正弦：从信号出现后 0.5 s 起，检查 `window_seconds` 秒。
///
/// 传入播放设备时录的是它的环回，即这个播放端点实际收到的声音。
pub fn capture_sine(device: &wasapi::Device, seconds: f64, window_seconds: f64, amplitude: f64) -> Result<Analysis> {
    let mut client = device.get_iaudioclient()?;
    let format = client.get_mixformat()?;
    require_float32(&format)?;
    let (default_period, _) = client.get_device_period()?;
    // 环回流在部分系统上不触发事件，所以统一用轮询
    client.initialize_client(
        &format,
        wasapi::Direction::Capture,
        StreamMode::PollingShared {
            autoconvert: false,
            buffer_duration_hns: default_period * 4,
        },
        None,
    )?;
    let capture_client = client.get_audiocaptureclient()?;
    let block_align = format.get_blockalign() as usize;
    let sample_rate = format.get_samplespersec();
    let rate = f64::from(sample_rate);
    let coefficient = 2.0 * (TAU * SINE_HZ / rate).cos();
    let threshold = amplitude * GLITCH_RATIO;

    let mut analysis = Analysis {
        sample_rate,
        ..Analysis::default()
    };
    let (mut previous, mut before_previous) = (0.0f64, 0.0f64);
    let mut index = 0u64;
    let mut signal_start: Option<u64> = None;
    let mut last_glitch: Option<u64> = None;
    let settle = (SETTLE_SECONDS * rate) as u64;
    let window = (window_seconds * rate) as u64;
    let mut buffer = Vec::new();

    client.start_stream()?;
    let started = Instant::now();
    while started.elapsed().as_secs_f64() < seconds {
        thread::sleep(POLL_INTERVAL);
        while let Some(frames) = capture_client.get_next_packet_size()?.filter(|frames| *frames > 0) {
            buffer.resize(frames as usize * block_align, 0);
            let (read, info) = capture_client.read_from_device(&mut buffer)?;
            if info.flags.data_discontinuity {
                analysis.discontinuities += 1;
            }
            for frame in buffer[..read as usize * block_align].chunks_exact(block_align) {
                let sample = if info.flags.silent {
                    0.0
                } else {
                    f64::from(f32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]))
                };
                if signal_start.is_none() && sample.abs() > amplitude * SIGNAL_RATIO {
                    signal_start = Some(index);
                }
                if let Some(start) = signal_start {
                    let offset = index - start;
                    if offset >= settle && offset < settle + window {
                        analysis.checked += 1;
                        analysis.peak = analysis.peak.max(sample.abs());
                        let residual = sample - (coefficient * previous - before_previous);
                        let in_holdoff = last_glitch.is_some_and(|last| index - last < GLITCH_HOLDOFF);
                        if residual.abs() > threshold && !in_holdoff {
                            analysis.glitches += 1;
                            if analysis.glitches <= 5 {
                                println!("glitch at {:.3} s, residual {residual:.5}", index as f64 / rate);
                            }
                            last_glitch = Some(index);
                        }
                    }
                }
                before_previous = previous;
                previous = sample;
                index += 1;
            }
        }
    }
    client.stop_stream()?;
    Ok(analysis)
}

/// 录音 `seconds` 秒，返回左声道的峰值（只用来确认有信号通过）。
pub fn capture_peak(device: &wasapi::Device, seconds: f64) -> Result<f64> {
    let mut client = device.get_iaudioclient()?;
    let format = client.get_mixformat()?;
    require_float32(&format)?;
    let (default_period, _) = client.get_device_period()?;
    client.initialize_client(
        &format,
        wasapi::Direction::Capture,
        StreamMode::PollingShared {
            autoconvert: false,
            buffer_duration_hns: default_period * 4,
        },
        None,
    )?;
    let capture_client = client.get_audiocaptureclient()?;
    let block_align = format.get_blockalign() as usize;
    let mut peak = 0.0f64;
    let mut buffer = Vec::new();
    client.start_stream()?;
    let started = Instant::now();
    while started.elapsed().as_secs_f64() < seconds {
        thread::sleep(POLL_INTERVAL);
        while let Some(frames) = capture_client.get_next_packet_size()?.filter(|frames| *frames > 0) {
            buffer.resize(frames as usize * block_align, 0);
            let (read, info) = capture_client.read_from_device(&mut buffer)?;
            if info.flags.silent {
                continue;
            }
            for frame in buffer[..read as usize * block_align].chunks_exact(block_align) {
                peak = peak.max(f64::from(f32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]])).abs());
            }
        }
    }
    client.stop_stream()?;
    Ok(peak)
}

/// 一段录音的左声道。
pub struct Recording {
    pub sample_rate: u32,
    pub left: Vec<f32>,
    /// WASAPI 报告"数据不连续"的位置（秒）：录音应用自己没来得及读、缓冲溢出，与被测引擎无关
    pub discontinuities: Vec<f64>,
    /// 真正开始录音的时刻（查找、初始化设备之后），录音中的时间都从这里算起
    pub started_at: Instant,
}

impl Recording {
    /// 把以 `origin` 为起点的时间换算成录音中的位置（秒）
    pub fn seconds_since(&self, origin: Instant, at_seconds: f64) -> f64 {
        at_seconds - self.started_at.saturating_duration_since(origin).as_secs_f64()
    }
}

/// 录音 `seconds` 秒，保存左声道（传入播放设备时录的是它的环回）。
pub fn record(device: &wasapi::Device, seconds: f64) -> Result<Recording> {
    let mut client = device.get_iaudioclient()?;
    let format = client.get_mixformat()?;
    require_float32(&format)?;
    let (default_period, _) = client.get_device_period()?;
    client.initialize_client(
        &format,
        wasapi::Direction::Capture,
        StreamMode::PollingShared {
            autoconvert: false,
            buffer_duration_hns: default_period * 4,
        },
        None,
    )?;
    let capture_client = client.get_audiocaptureclient()?;
    let block_align = format.get_blockalign() as usize;
    let sample_rate = format.get_samplespersec();
    let mut left = Vec::with_capacity((seconds * f64::from(sample_rate)) as usize);
    let mut discontinuities = Vec::new();
    let mut buffer = Vec::new();
    client.start_stream()?;
    let started = Instant::now();
    while started.elapsed().as_secs_f64() < seconds {
        thread::sleep(POLL_INTERVAL);
        while let Some(frames) = capture_client.get_next_packet_size()?.filter(|frames| *frames > 0) {
            buffer.resize(frames as usize * block_align, 0);
            let (read, info) = capture_client.read_from_device(&mut buffer)?;
            if info.flags.data_discontinuity {
                discontinuities.push(left.len() as f64 / f64::from(sample_rate));
            }
            for frame in buffer[..read as usize * block_align].chunks_exact(block_align) {
                left.push(if info.flags.silent {
                    0.0
                } else {
                    f32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]])
                });
            }
        }
    }
    client.stop_stream()?;
    Ok(Recording {
        sample_rate,
        left,
        discontinuities,
        started_at: started,
    })
}

/// 检查录音中 [`from_s`, `to_s`) 这段（从录音开始算起）是否是连续的 1 kHz 正弦。
pub fn check_sine(recording: &Recording, from_s: f64, to_s: f64, amplitude: f64) -> Analysis {
    let rate = f64::from(recording.sample_rate);
    let coefficient = 2.0 * (TAU * SINE_HZ / rate).cos();
    let threshold = amplitude * GLITCH_RATIO;
    let from = ((from_s * rate) as usize).max(2).min(recording.left.len());
    let to = ((to_s * rate) as usize).min(recording.left.len());
    let mut analysis = Analysis {
        sample_rate: recording.sample_rate,
        ..Analysis::default()
    };
    let mut last_glitch: Option<usize> = None;
    for index in from..to {
        let sample = f64::from(recording.left[index]);
        let residual =
            sample - (coefficient * f64::from(recording.left[index - 1]) - f64::from(recording.left[index - 2]));
        analysis.checked += 1;
        analysis.peak = analysis.peak.max(sample.abs());
        let in_holdoff = last_glitch.is_some_and(|last| ((index - last) as u64) < GLITCH_HOLDOFF);
        if residual.abs() > threshold && !in_holdoff {
            analysis.glitches += 1;
            if analysis.glitches <= 5 {
                println!("glitch at {:.3} s, residual {residual:.5}", index as f64 / rate);
            }
            last_glitch = Some(index);
        }
    }
    analysis
}
