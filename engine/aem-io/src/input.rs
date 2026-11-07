//! 外部输入（录音端点）的 IO 线程：设备 → 立体声 → ASRC 到引擎采样率 → 环形缓冲。
//!
//! 重采样在生产方（本线程）进行，固定每次处理 1 ms 的设备帧；调度线程只按块取数据。
//! 控制器的反馈量是环形缓冲的水位：水位偏高说明设备时钟比引擎快，就把比率调低、少产出一些。

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use aem_dsp::resample::{FixedSide, RateController, StereoResampler, relative_ratio};
use rtrb::Producer;

use crate::bridge::{BridgeError, BridgeLog, CONTROL_PERIOD};
use crate::device::{self, EventWatch, OpenStream, SessionEnd, Waited};
use crate::{BridgeConfig, BridgeHandle, IoDirection, STEREO, StatusSink, TARGET_LEVEL_MS, frames_to_ms};

/// 每次重采样处理的设备帧数：1 ms
const CHUNKS_PER_SECOND: u32 = 1000;
/// 读设备数据的初始缓冲：100 ms，不够时再增长
const INITIAL_READ_MS: u32 = 100;

pub fn spawn_input(config: BridgeConfig, producer: Producer<f32>, status: StatusSink) -> io::Result<BridgeHandle> {
    let stop = device::stop_flag();
    let thread_stop = stop.clone();
    let thread = thread::Builder::new()
        .name("aem-input".to_owned())
        .spawn(move || run(&config, producer, &thread_stop, &status))?;
    Ok(BridgeHandle::new(stop, thread))
}

fn run(config: &BridgeConfig, mut producer: Producer<f32>, stop: &AtomicBool, status: &StatusSink) {
    let _priority = audio_thread_priority::promote_current_thread_to_real_time(0, config.engine_rate.hz())
        .inspect_err(|error| tracing::warn!(?error, "cannot raise the input thread priority"));
    device::run_with_retry(&config.endpoint, IoDirection::Input, stop, status, |stream| {
        session(config, stream, &mut producer, stop)
    });
}

fn session(config: &BridgeConfig, stream: &OpenStream, producer: &mut Producer<f32>, stop: &AtomicBool) -> SessionEnd {
    match run_session(config, stream, producer, stop) {
        Ok(end) => end,
        Err(error) => SessionEnd::failed(&error),
    }
}

fn run_session(
    config: &BridgeConfig,
    stream: &OpenStream,
    producer: &mut Producer<f32>,
    stop: &AtomicBool,
) -> Result<SessionEnd, BridgeError> {
    let capture = stream.client.get_audiocaptureclient()?;
    let device_rate = stream.sample_rate;
    let chunk_frames = (device_rate.hz() / CHUNKS_PER_SECOND).max(1) as usize;
    let ratio = f64::from(config.engine_rate.hz()) / f64::from(device_rate.hz());
    let mut resampler = StereoResampler::new(config.quality, ratio, chunk_frames, FixedSide::Input)?;
    let mut bytes = vec![0u8; stream.block_align * (device_rate.hz() / (1000 / INITIAL_READ_MS)) as usize];
    let mut staging: Vec<f32> = Vec::with_capacity(bytes.len() / stream.block_align * STEREO);
    let mut produced: Vec<f32> = Vec::with_capacity(resampler.output_frames_max() * STEREO * 8);
    let capacity = producer.buffer().capacity();
    let mut controller = RateController::new(TARGET_LEVEL_MS);
    let mut log = BridgeLog::new(&config.endpoint.0);

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

        while let Some(frames) = capture.get_next_packet_size()?.filter(|frames| *frames > 0) {
            let len = frames as usize * stream.block_align;
            if bytes.len() < len {
                bytes.resize(len, 0);
            }
            let (read, info) = capture.read_from_device(&mut bytes[..len])?;
            if info.flags.data_discontinuity {
                log.discontinuities += 1;
            }
            let read_len = read as usize * stream.block_align;
            if info.flags.silent {
                staging.resize(staging.len() + read as usize * STEREO, 0.0);
            } else {
                stream.append_stereo(&bytes[..read_len], &mut staging);
            }
        }

        let mut consumed = 0;
        loop {
            let needed = resampler.input_frames_next() * STEREO;
            if staging.len() - consumed < needed {
                break;
            }
            resampler.process(&staging[consumed..consumed + needed], &mut produced)?;
            consumed += needed;
        }
        staging.drain(..consumed);

        let writable = producer.slots().min(produced.len());
        if let Ok(chunk) = producer.write_chunk_uninit(writable) {
            chunk.fill_from_iter(produced.iter().copied());
        }
        let dropped = produced.len() - writable;
        if dropped > 0 {
            log.overflow(dropped / STEREO);
        }
        produced.clear();

        let level_ms = frames_to_ms((capacity - producer.slots()) / STEREO, config.engine_rate);
        log.level(level_ms);
        if log.control_due() {
            let update = controller.update(log.take_average(), CONTROL_PERIOD.as_secs_f64());
            resampler.set_relative_ratio(relative_ratio(update.correction_ppm))?;
            log.after_control(update);
        }
    }
}
