//! 外部输入（录音端点）的 IO 线程：设备 → 立体声 → ASRC 到引擎采样率 → 环形缓冲（[`crate::capture`]）。

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use rtrb::Producer;

use crate::bridge::{BridgeError, BufferLevel};
use crate::capture::CapturePump;
use crate::clock::{ClockReader, ClockTap};
use crate::device::{self, EventWatch, IoContext, OpenStream, SessionEnd, Waited};
use crate::{BridgeConfig, BridgeHandle, IoDirection};

/// 启动一台外部输入的 IO 线程：打开设备，把录到的声音转成引擎采样率写进 `producer`，
/// 出错时报告离线并按节奏重试，直到 [`BridgeHandle`] 被丢弃。
pub fn spawn_input(
    config: BridgeConfig,
    producer: Producer<f32>,
    context: IoContext,
    clock: ClockTap,
) -> io::Result<BridgeHandle> {
    let stop = device::stop_flag();
    let thread_stop = stop.clone();
    let level = Arc::new(BufferLevel::default());
    let thread_level = level.clone();
    let thread = thread::Builder::new()
        .name("aemeath-input".to_owned())
        .spawn(move || run(&config, producer, &thread_stop, &context, &clock, &thread_level))?;
    Ok(BridgeHandle::new(stop, level, thread))
}

/// 输入线程的主体：提高优先级后反复打开设备运行会话，直到收到停止信号。
fn run(
    config: &BridgeConfig,
    mut producer: Producer<f32>,
    stop: &AtomicBool,
    context: &IoContext,
    clock: &ClockTap,
    level: &Arc<BufferLevel>,
) {
    let _priority = aemeath_sys::thread_priority::promote_current_thread()
        .inspect_err(|error| tracing::warn!(%error, "cannot raise the input thread priority"));
    device::run_with_retry(
        &config.request(IoDirection::Input),
        stop,
        context,
        |stream, online| match run_session(config, stream, online, &mut producer, stop, clock, level) {
            Ok(end) => end,
            Err(error) => SessionEnd::failed(&error),
        },
    );
}

fn run_session(
    config: &BridgeConfig,
    stream: &OpenStream,
    online: &dyn Fn(),
    producer: &mut Producer<f32>,
    stop: &AtomicBool,
    clock: &ClockTap,
    level: &Arc<BufferLevel>,
) -> Result<SessionEnd, BridgeError> {
    let capture = stream.client.get_audiocaptureclient()?;
    let mut clock_reader = ClockReader::new(&stream.client)?;
    let mut pump = CapturePump::new(
        stream,
        config.engine_rate,
        config.quality,
        producer,
        &config.endpoint.0,
        level.clone(),
    )?;

    stream.client.start_stream()?;
    let mut watch = EventWatch::new(online);
    loop {
        if stop.load(Ordering::Relaxed) || producer.is_abandoned() {
            let _ = stream.client.stop_stream();
            return Ok(SessionEnd::Finished);
        }
        if let Waited::Timeout = watch.wait(stream)? {
            continue;
        }
        pump.read_packets(&capture)?;
        clock_reader.poll(clock);
        pump.deliver(producer)?;
    }
}
