//! 外部输出（播放端点）的 IO 线程：环形缓冲 → ASRC 到设备采样率 → 设备。
//!
//! 重采样在消费方（本线程）进行，固定每次产出 1 ms 的设备帧，放进一个小队列，再按设备每次要的帧数写出。
//! 水位 = 环形缓冲中的引擎帧 + 两级重采样之间的帧 + 已重采样但尚未写给设备的帧。
//! 水位偏高说明引擎比设备快，就把比率调低、多消耗输入。FFT 要攒够一批才有产出，目标水位相应加上一批的时长。

use std::collections::VecDeque;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use aemeath_dsp::resample::{BridgeResampler, FixedSide, RateController, relative_ratio};
use rtrb::Consumer;

use crate::bridge::{BridgeError, BridgeLog, CONTROL_PERIOD};
use crate::clock::{ClockReader, ClockTap};
use crate::device::{self, EventWatch, IoContext, OpenStream, SessionEnd, Waited};
use crate::{
    BridgeConfig, BridgeHandle, IoDirection, STEREO, TARGET_LEVEL_MS, frames_to_ms, ms_to_frames, skip_backlog,
};

/// 每次重采样产出的设备帧数：1 ms
const CHUNKS_PER_SECOND: u32 = 1000;
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
    context: IoContext,
    clock: ClockTap,
) -> io::Result<BridgeHandle> {
    let stop = device::stop_flag();
    let thread_stop = stop.clone();
    let thread = thread::Builder::new()
        .name("aemeath-output".to_owned())
        .spawn(move || run(&config, consumer, &active, &thread_stop, &context, &clock))?;
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

/// 输出线程的主体：提高优先级后反复打开设备运行会话，直到收到停止信号。会话期间 `active` 为 true。
fn run(
    config: &BridgeConfig,
    mut consumer: Consumer<f32>,
    active: &AtomicBool,
    stop: &AtomicBool,
    context: &IoContext,
    clock: &ClockTap,
) {
    let _priority = aemeath_sys::thread_priority::promote_current_thread()
        .inspect_err(|error| tracing::warn!(%error, "cannot raise the output thread priority"));
    device::run_with_retry(&config.request(IoDirection::Output), stop, context, |stream| {
        let _active = ActiveGuard::set(active);
        match run_session(config, stream, &mut consumer, stop, clock) {
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
    clock: &ClockTap,
) -> Result<SessionEnd, BridgeError> {
    let render = stream.client.get_audiorenderclient()?;
    let mut clock_reader = ClockReader::new(&stream.client)?;
    let mut codec = stream.frame_codec();
    let device_rate = stream.sample_rate();
    let chunk_frames = (device_rate.hz() / CHUNKS_PER_SECOND).max(1) as usize;
    let mut resampler = BridgeResampler::new(
        config.quality,
        config.engine_rate,
        device_rate,
        chunk_frames,
        FixedSide::Output,
    )?;
    let buffer_frames = stream.client.get_buffer_size()? as usize;
    let mut input = vec![0.0f32; resampler.input_frames_next() * STEREO * 2];
    let mut resampled: Vec<f32> = Vec::with_capacity(resampler.output_frames_max() * STEREO);
    let mut queue: VecDeque<f32> =
        VecDeque::with_capacity((buffer_frames + resampler.output_frames_max() * 2) * STEREO * 2);
    let mut stereo: Vec<f32> = Vec::with_capacity(buffer_frames * STEREO);
    let mut bytes: Vec<u8> = Vec::with_capacity(buffer_frames * stream.block_align);
    let target_ms = TARGET_LEVEL_MS + resampler.burst_ms();
    let mut controller = RateController::new(target_ms);
    let mut log = BridgeLog::new(&config.endpoint.0);

    // 先攒够目标水位再开流，控制器从零误差开始
    let prime_samples = ms_to_frames(target_ms, config.engine_rate) * STEREO;
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
    // 先写满一个缓冲的静音再开流（独占模式要求开流前预先填好一个周期）
    let available = stream.client.get_available_space_in_frames()? as usize;
    codec.write_stereo(&vec![0.0; available * STEREO], &mut bytes);
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

        let skipped = skip_backlog(consumer, prime_samples);
        if skipped > 0 {
            log.overflow(skipped / STEREO);
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
            let started = Instant::now();
            resampler.process(&input[..needed], &mut resampled)?;
            log.block_processed(started.elapsed());
            queue.extend(resampled.iter().copied());
        }

        stereo.clear();
        stereo.extend(queue.drain(..available * STEREO));
        codec.write_stereo(&stereo, &mut bytes);
        render.write_to_device(&bytes, None)?;
        clock_reader.poll(clock);

        let level_ms = frames_to_ms(
            consumer.slots() / STEREO + resampler.pending_frames(),
            config.engine_rate,
        ) + frames_to_ms(queue.len() / STEREO, device_rate);
        log.level(level_ms);
        if log.control_due() {
            let update = controller.update(log.take_average(), CONTROL_PERIOD.as_secs_f64());
            resampler.set_relative_ratio(relative_ratio(update.correction_ppm))?;
            log.after_control(update);
        }
    }
}
