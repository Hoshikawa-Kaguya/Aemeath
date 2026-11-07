//! 外部输出（播放端点）的 IO 线程：环形缓冲 → ASRC 到设备采样率 → 设备（S5 的播放线程）。
//!
//! 重采样在消费方（本线程）进行，固定每次产出 5 ms 的设备帧，放进一个小队列，再按设备每次要的帧数写出。
//! 水位 = 环形缓冲中的引擎帧 + 已重采样但尚未写给设备的帧。水位偏高说明引擎比设备快，就把比率调低、多消耗输入。

use std::collections::VecDeque;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use aemeath_dsp::resample::{FixedSide, RateController, StereoResampler, relative_ratio};
use rtrb::Consumer;

use crate::bridge::{BridgeError, BridgeLog, CONTROL_PERIOD};
use crate::device::{self, EventWatch, OpenStream, SessionEnd, Waited};
use crate::{
    BridgeConfig, BridgeHandle, IoDirection, STEREO, StatusSink, TARGET_LEVEL_MS, frames_to_ms, target_frames,
};

/// 每次重采样产出的设备帧数：5 ms（S5）
const CHUNKS_PER_SECOND: u32 = 200;
/// 开流前等待环形缓冲攒够目标水位时的轮询间隔
const PRIME_POLL: Duration = Duration::from_millis(1);

/// 启动外部输出的 IO 线程。
///
/// `active`：设备打开期间为 true。引擎只在它为 true 时往环形缓冲里写，这样设备打开之前和离线期间
/// 既不会积压旧数据，也不会把"没有人消费"误报成丢数据。
pub fn spawn_output(
    config: BridgeConfig,
    consumer: Consumer<f32>,
    active: Arc<AtomicBool>,
    status: StatusSink,
) -> io::Result<BridgeHandle> {
    let stop = device::stop_flag();
    let thread_stop = stop.clone();
    let thread = thread::Builder::new()
        .name("aemeath-output".to_owned())
        .spawn(move || run(&config, consumer, &active, &thread_stop, &status))?;
    Ok(BridgeHandle::new(stop, thread))
}

/// 会话期间把 `active` 置为 true，会话结束（包括出错）时清除。
struct ActiveGuard<'a>(&'a AtomicBool);

impl<'a> ActiveGuard<'a> {
    fn set(flag: &'a AtomicBool) -> Self {
        flag.store(true, Ordering::Release);
        Self(flag)
    }
}

impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn run(
    config: &BridgeConfig,
    mut consumer: Consumer<f32>,
    active: &AtomicBool,
    stop: &AtomicBool,
    status: &StatusSink,
) {
    let _priority = audio_thread_priority::promote_current_thread_to_real_time(0, config.engine_rate.hz())
        .inspect_err(|error| tracing::warn!(?error, "cannot raise the output thread priority"));
    device::run_with_retry(&config.endpoint, IoDirection::Output, stop, status, |stream| {
        let _active = ActiveGuard::set(active);
        match run_session(config, stream, &mut consumer, stop) {
            Ok(end) => end,
            Err(error) => SessionEnd::failed(&error),
        }
    });
}

fn run_session(
    config: &BridgeConfig,
    stream: &OpenStream,
    consumer: &mut Consumer<f32>,
    stop: &AtomicBool,
) -> Result<SessionEnd, BridgeError> {
    let render = stream.client.get_audiorenderclient()?;
    let device_rate = stream.sample_rate;
    let chunk_frames = (device_rate.hz() / CHUNKS_PER_SECOND).max(1) as usize;
    let ratio = f64::from(device_rate.hz()) / f64::from(config.engine_rate.hz());
    let mut resampler = StereoResampler::new(config.quality, ratio, chunk_frames, FixedSide::Output)?;
    let buffer_frames = stream.client.get_buffer_size()? as usize;
    let mut input = vec![0.0f32; resampler.input_frames_next() * STEREO * 2];
    let mut resampled: Vec<f32> = Vec::with_capacity(resampler.output_frames_max() * STEREO);
    let mut queue: VecDeque<f32> = VecDeque::with_capacity((buffer_frames + chunk_frames * 2) * STEREO * 2);
    let mut stereo: Vec<f32> = Vec::with_capacity(buffer_frames * STEREO);
    let mut bytes: Vec<u8> = Vec::with_capacity(buffer_frames * stream.block_align);
    let mut controller = RateController::new(TARGET_LEVEL_MS);
    let mut log = BridgeLog::new(&config.endpoint.0);

    // 先攒够目标水位再开流，控制器从零误差开始
    let prime_samples = target_frames(config.engine_rate) * STEREO;
    while consumer.slots() < prime_samples {
        if stop.load(Ordering::Relaxed) || consumer.is_abandoned() {
            return Ok(SessionEnd::Finished);
        }
        thread::sleep(PRIME_POLL);
    }
    // 设备离线期间引擎仍在写入，缓冲里可能积压了几百毫秒的旧数据；丢到目标水位，避免重新上线后延迟一直偏大
    let backlog = consumer.slots() - prime_samples;
    if backlog > 0
        && let Ok(chunk) = consumer.read_chunk(backlog)
    {
        chunk.commit_all();
    }
    let available = stream.client.get_available_space_in_frames()? as usize;
    stream.write_stereo(&vec![0.0; available * STEREO], &mut bytes);
    render.write_to_device(&bytes, None)?;
    stream.client.start_stream()?;

    let mut watch = EventWatch::new();
    loop {
        if stop.load(Ordering::Relaxed) || consumer.is_abandoned() {
            let _ = stream.client.stop_stream();
            return Ok(SessionEnd::Finished);
        }
        if let Waited::Timeout = watch.wait(stream)? {
            continue;
        }

        let available = stream.client.get_available_space_in_frames()? as usize;
        while queue.len() / STEREO < available {
            let needed = resampler.input_frames_next() * STEREO;
            if consumer.slots() < needed {
                // 欠载：用静音补足本次所需，不消耗输入，等引擎把水位补回来
                log.underflow();
                let missing = available - queue.len() / STEREO;
                queue.extend(std::iter::repeat_n(0.0, missing * STEREO));
                break;
            }
            if input.len() < needed {
                input.resize(needed, 0.0);
            }
            if let Ok(chunk) = consumer.read_chunk(needed) {
                let (first, second) = chunk.as_slices();
                input[..first.len()].copy_from_slice(first);
                input[first.len()..first.len() + second.len()].copy_from_slice(second);
                chunk.commit_all();
            }
            resampled.clear();
            resampler.process(&input[..needed], &mut resampled)?;
            queue.extend(resampled.iter().copied());
        }

        stereo.clear();
        stereo.extend(queue.drain(..available * STEREO));
        stream.write_stereo(&stereo, &mut bytes);
        render.write_to_device(&bytes, None)?;

        let level_ms = frames_to_ms(consumer.slots() / STEREO, config.engine_rate)
            + frames_to_ms(queue.len() / STEREO, device_rate);
        log.level(level_ms);
        if log.control_due() {
            let update = controller.update(log.take_average(), CONTROL_PERIOD.as_secs_f64());
            resampler.set_relative_ratio(relative_ratio(update.correction_ppm))?;
            log.after_control(update);
        }
    }
}
