//! 外部输入（录音端点）的 IO 线程：设备 → 立体声 → ASRC 到引擎采样率 → 环形缓冲（[`crate::capture`]）。

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use rtrb::Producer;

use crate::bridge::BridgeError;
use crate::capture::CapturePump;
use crate::clock::{ClockReader, ClockTap};
use crate::device::{self, EventWatch, IoContext, OpenStream, SessionEnd, Waited};
use crate::{BridgeConfig, BridgeHandle, IoDirection};

pub fn spawn_input(
    config: BridgeConfig,
    producer: Producer<f32>,
    context: IoContext,
    clock: ClockTap,
) -> io::Result<BridgeHandle> {
    let stop = device::stop_flag();
    let thread_stop = stop.clone();
    let thread = thread::Builder::new()
        .name("aemeath-input".to_owned())
        .spawn(move || run(&config, producer, &thread_stop, &context, &clock))?;
    Ok(BridgeHandle::new(stop, thread))
}

fn run(config: &BridgeConfig, mut producer: Producer<f32>, stop: &AtomicBool, context: &IoContext, clock: &ClockTap) {
    let _priority = aemeath_sys::thread_priority::promote_current_thread()
        .inspect_err(|error| tracing::warn!(%error, "cannot raise the input thread priority"));
    device::run_with_retry(
        &config.request(IoDirection::Input),
        stop,
        context,
        |stream| match run_session(config, stream, &mut producer, stop, clock) {
            Ok(end) => end,
            Err(error) => SessionEnd::failed(&error),
        },
    );
}

fn run_session(
    config: &BridgeConfig,
    stream: &OpenStream,
    producer: &mut Producer<f32>,
    stop: &AtomicBool,
    clock: &ClockTap,
) -> Result<SessionEnd, BridgeError> {
    let capture = stream.client.get_audiocaptureclient()?;
    let mut clock_reader = ClockReader::new(&stream.client)?;
    let mut pump = CapturePump::new(stream, config.engine_rate, config.quality, producer, &config.endpoint.0)?;

    stream.client.start_stream()?;
    let mut watch = EventWatch::new();
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
