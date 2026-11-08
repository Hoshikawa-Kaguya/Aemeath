//! 诊断：独占打开一个录音设备，统计 2 秒内的设备事件数和读到的包数（事件驱动与轮询各看一遍）。
//!
//! 用法：`cargo run -p aemeath-io --example exclusive-capture -- <设备名的一部分>`

use std::time::{Duration, Instant};

use aemeath_io::IoDirection;
use aemeath_io::capabilities::CapabilityCache;
use aemeath_io::device::{DeviceRequest, open};
use aemeath_protocol::ids::EndpointId;
use aemeath_protocol::message::{DeviceFormat, SampleFormat};
use aemeath_protocol::topology::{BufferChoice, DeviceAccess, FormatChoice};
use aemeath_protocol::units::SampleRate;
use wasapi::{DeviceEnumerator, Direction};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const RUN: Duration = Duration::from_secs(2);
const WAIT_MS: u32 = 100;

fn main() -> Result<()> {
    let name = std::env::args().nth(1).unwrap_or_else(|| "Rear Mic".to_owned());
    wasapi::initialize_mta().ok()?;
    let collection = DeviceEnumerator::new()?.get_device_collection(Direction::Capture)?;
    let device = (&collection)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .find(|device| device.get_friendlyname().is_ok_and(|friendly| friendly.contains(&name)))
        .ok_or("device not found")?;
    // 第二个参数为 int16 时固定用 48 kHz · 16 位，否则用推荐格式
    let access = match std::env::args().nth(2).as_deref() {
        Some("int16") => DeviceAccess::Exclusive {
            format: FormatChoice::Fixed {
                format: DeviceFormat {
                    sample_rate: SampleRate::HZ_48000,
                    sample_format: SampleFormat::Int16,
                    channels: 2,
                },
            },
            buffer: BufferChoice::DeviceDefault,
        },
        _ => DeviceAccess::default(),
    };
    let request = DeviceRequest {
        endpoint: EndpointId(device.get_id()?),
        direction: IoDirection::Input,
        access,
        engine_rate: SampleRate::HZ_48000,
    };
    let stream = match open(&request, &CapabilityCache::default()) {
        Ok(stream) => stream,
        Err(_) => return Err("cannot open the device exclusively".into()),
    };
    if std::env::args().nth(3).as_deref() == Some("poll") {
        let format = stream.format;
        // 先释放事件驱动的独占流，否则第二条独占流打不开
        drop(stream);
        return poll(&device, format);
    }
    let (default_period, minimum_period) = stream.client.get_device_period()?;
    println!(
        "opened {:?}, buffer {} frames, periods default {default_period} min {minimum_period} (100 ns)",
        stream.format,
        stream.client.get_buffer_size()?
    );
    let capture = stream.client.get_audiocaptureclient()?;
    stream.client.start_stream()?;
    let started = Instant::now();
    let (mut events, mut timeouts, mut packets, mut frames) = (0, 0, 0, 0u64);
    let mut bytes = vec![0u8; 1 << 20];
    while started.elapsed() < RUN {
        match stream.event.wait_for_event(WAIT_MS) {
            Ok(()) => events += 1,
            Err(_) => timeouts += 1,
        }
        while let Some(size) = capture.get_next_packet_size()?.filter(|size| *size > 0) {
            let length = size as usize * stream.block_align;
            let (read, _) = capture.read_from_device(&mut bytes[..length])?;
            packets += 1;
            frames += u64::from(read);
        }
    }
    stream.client.stop_stream()?;
    println!("events {events}, timeouts {timeouts}, packets {packets}, frames {frames} in {RUN:?}");
    Ok(())
}

/// 用独占的轮询模式（不用事件）再打开一次，看设备是否交付数据。
fn poll(device: &wasapi::Device, format: DeviceFormat) -> Result<()> {
    let mut client = device.get_iaudioclient()?;
    let wave = aemeath_io::format::to_wave_format(format);
    let (default_period, _) = client.get_device_period()?;
    client.initialize_client(
        &wave,
        Direction::Capture,
        wasapi::StreamMode::PollingExclusive {
            buffer_duration_hns: default_period * 4,
            period_hns: default_period,
        },
        None,
    )?;
    let capture = client.get_audiocaptureclient()?;
    client.start_stream()?;
    let started = Instant::now();
    let (mut packets, mut frames) = (0, 0u64);
    let mut bytes = vec![0u8; 1 << 20];
    let block_align = wave.get_blockalign() as usize;
    while started.elapsed() < RUN {
        std::thread::sleep(Duration::from_millis(5));
        while let Some(size) = capture.get_next_packet_size()?.filter(|size| *size > 0) {
            let (read, _) = capture.read_from_device(&mut bytes[..size as usize * block_align])?;
            packets += 1;
            frames += u64::from(read);
        }
    }
    client.stop_stream()?;
    println!("polling: packets {packets}, frames {frames} in {RUN:?}");
    Ok(())
}
