//! 录音流共用的一段：读包 → 转立体声 → ASRC → 环形缓冲 → 按缓冲占用调整比率。外部输入和应用源都用它。
//!
//! 重采样在生产方（IO 线程）进行，固定每次处理 1 ms 的设备帧；调度线程只按块取数据。
//! 控制器的反馈量是环形缓冲的占用：缓冲占用偏高说明设备时钟比引擎快，就把比率调低、少产出一些。
//!
//! FFT 的产出成批到达，缓冲占用呈锯齿形：第一包数据到达时先写入"目标占用 + 一批"的静音，引擎的启动门槛不用改，
//! 锯齿的谷值也不低于目标占用；控制目标取锯齿的平均值。共享模式的目标和预填充再加上余量
//! （[`OpenStream::level_margin_ms`]）。

use std::sync::Arc;
use std::time::Instant;

use aemeath_dsp::resample::{BridgeResampler, FixedSide, RateController, relative_ratio};
use aemeath_protocol::topology::Resampler;
use aemeath_protocol::units::SampleRate;
use rtrb::Producer;
use wasapi::AudioCaptureClient;

use crate::bridge::{BridgeError, BridgeLog, BufferLevel, CONTROL_PERIOD};
use crate::device::{FrameCodec, OpenStream};
use crate::{STEREO, TARGET_LEVEL_MS, frames_to_ms, ms_to_frames};

/// 每次重采样处理的设备帧数：1 ms
const CHUNKS_PER_SECOND: u32 = 1000;
/// 读设备数据的初始缓冲：100 ms，不够时再增长
const INITIAL_READ_MS: u32 = 100;

/// 一路录音流从设备到环形缓冲的搬运者，状态（重采样器、控制器、统计）跨多次读包保留。
pub struct CapturePump {
    codec: FrameCodec,
    block_align: usize,
    /// 独占时设备缓冲的帧数（每次最多交付这么多），共享时为 0
    exclusive_frames: u32,
    engine_rate: SampleRate,
    resampler: BridgeResampler,
    bytes: Vec<u8>,
    staging: Vec<f32>,
    produced: Vec<f32>,
    capacity: usize,
    controller: RateController,
    log: BridgeLog,
    prefill: Prefill,
}

/// 第一包数据到达时才写入的静音预填充。
///
/// 开流到第一包到达要一段时间（实测共享模式约 20 ms），引擎期间一直在读：开流前就写好的话，
/// 引擎用这段静音启动，第一包到达前就读空了，重新启动后缓冲少了共享模式的余量，要等控制器花几十秒慢慢补回来，
/// 期间设备交付稍有抖动就再读空一次。等到有数据再写，引擎在此之前没有启动，不算读空。
struct Prefill {
    samples: usize,
    pending: bool,
}

impl Prefill {
    /// 第一次有数据要写时先写入预填充；还在等第一包数据时返回 false。
    fn ready(&mut self, producer: &mut Producer<f32>, has_data: bool) -> bool {
        if !self.pending {
            return true;
        }
        if !has_data {
            return false;
        }
        self.pending = false;
        let samples = self.samples.min(producer.slots());
        if samples > 0
            && let Ok(chunk) = producer.write_chunk_uninit(samples)
        {
            chunk.fill_from_iter(std::iter::repeat(0.0));
        }
        true
    }
}

impl CapturePump {
    /// `label` 只用于日志（端点 ID 或程序路径）。
    pub fn new(
        stream: &OpenStream,
        engine_rate: SampleRate,
        quality: Resampler,
        producer: &mut Producer<f32>,
        label: &str,
        level: Arc<BufferLevel>,
    ) -> Result<Self, BridgeError> {
        let device_rate = stream.sample_rate();
        let chunk_frames = (device_rate.hz() / CHUNKS_PER_SECOND).max(1) as usize;
        let resampler = BridgeResampler::new(quality, device_rate, engine_rate, chunk_frames, FixedSide::Input)?;
        let burst_ms = resampler.burst_ms();
        let margin_ms = stream.level_margin_ms();
        let prefill = Prefill {
            samples: if burst_ms + margin_ms > 0.0 {
                ms_to_frames(TARGET_LEVEL_MS + margin_ms + burst_ms, engine_rate) * STEREO
            } else {
                0
            },
            pending: true,
        };
        let target_ms = TARGET_LEVEL_MS + margin_ms + burst_ms / 2.0;
        let bytes = vec![0u8; stream.block_align * (device_rate.hz() / (1000 / INITIAL_READ_MS)) as usize];
        let staging = Vec::with_capacity(bytes.len() / stream.block_align * STEREO);
        let produced = Vec::with_capacity(resampler.output_frames_max() * STEREO * 8);
        let exclusive_frames = if stream.exclusive {
            stream.client.get_buffer_size()?
        } else {
            0
        };
        Ok(Self {
            codec: stream.frame_codec(),
            block_align: stream.block_align,
            exclusive_frames,
            engine_rate,
            resampler,
            bytes,
            staging,
            produced,
            capacity: producer.buffer().capacity(),
            controller: RateController::new(target_ms),
            log: BridgeLog::new(label, target_ms, level),
            prefill,
        })
    }

    /// 取走设备已交付的全部包，重采样后写进环形缓冲。
    pub fn read_packets(&mut self, capture: &AudioCaptureClient) -> Result<(), BridgeError> {
        loop {
            // 独占模式下 wasapi 的 get_next_packet_size 固定返回 None：按整个缓冲读，读到 0 帧为止
            let frames = capture.get_next_packet_size()?.unwrap_or(self.exclusive_frames);
            if frames == 0 {
                break;
            }
            let len = frames as usize * self.block_align;
            if self.bytes.len() < len {
                self.bytes.resize(len, 0);
            }
            let (read, info) = capture.read_from_device(&mut self.bytes[..len])?;
            if read == 0 {
                break;
            }
            if info.flags.data_discontinuity {
                self.log.discontinuities += 1;
            }
            let read_len = read as usize * self.block_align;
            if info.flags.silent {
                self.staging.resize(self.staging.len() + read as usize * STEREO, 0.0);
            } else {
                self.codec.append_stereo(&self.bytes[..read_len], &mut self.staging);
            }
        }
        Ok(())
    }

    /// 把攒够的数据重采样并写进环形缓冲，按缓冲占用调整比率。
    ///
    /// 写入前后各记一次占用：写入前是锯齿的谷、写入后是峰，两者平均才是时间平均。
    /// 只记写入后的峰值时，控制器会把峰值拉到目标，实际平均占用低约半个设备周期。
    pub fn deliver(&mut self, producer: &mut Producer<f32>) -> Result<(), BridgeError> {
        let mut consumed = 0;
        loop {
            let needed = self.resampler.input_frames_next() * STEREO;
            if self.staging.len() - consumed < needed {
                break;
            }
            let started = Instant::now();
            self.resampler
                .process(&self.staging[consumed..consumed + needed], &mut self.produced)?;
            self.log.block_processed(started.elapsed());
            consumed += needed;
        }
        self.staging.drain(..consumed);

        // 第一包数据到达之前缓冲是空的，不记占用，否则统计里的最低占用是 0，控制器也会被带偏
        if !self.prefill.ready(producer, !self.produced.is_empty()) {
            return Ok(());
        }
        self.log.level(self.level_ms(producer));
        let writable = producer.slots().min(self.produced.len());
        if let Ok(chunk) = producer.write_chunk_uninit(writable) {
            chunk.fill_from_iter(self.produced.iter().copied());
        }
        let dropped = self.produced.len() - writable;
        if dropped > 0 {
            self.log.overflow(dropped / STEREO);
        }
        self.produced.clear();

        self.log.level(self.level_ms(producer));
        if self.log.control_due() {
            let update = self
                .controller
                .update(self.log.take_average(), CONTROL_PERIOD.as_secs_f64());
            self.resampler
                .set_relative_ratio(relative_ratio(update.correction_ppm))?;
            self.log.after_control(update);
        }
        Ok(())
    }

    /// 环形缓冲当前的占用（ms）。
    fn level_ms(&self, producer: &Producer<f32>) -> f64 {
        frames_to_ms((self.capacity - producer.slots()) / STEREO, self.engine_rate)
    }
}

#[cfg(test)]
mod tests {
    use aemeath_dsp::resample::{RateController, RateUpdate, relative_ratio};

    use rtrb::RingBuffer;

    use super::Prefill;
    use crate::{SHARED_MARGIN_MS, TARGET_LEVEL_MS};

    #[test]
    fn the_prefill_waits_for_the_first_data_and_is_written_once() {
        let (mut producer, consumer) = RingBuffer::<f32>::new(16);
        let mut prefill = Prefill {
            samples: 6,
            pending: true,
        };
        assert!(!prefill.ready(&mut producer, false), "no data yet");
        assert_eq!(consumer.slots(), 0, "the engine sees an empty ring until data arrives");
        assert!(prefill.ready(&mut producer, true));
        assert_eq!(consumer.slots(), 6);
        assert!(prefill.ready(&mut producer, false), "later calls pass through");
        assert!(prefill.ready(&mut producer, true));
        assert_eq!(consumer.slots(), 6, "written only once");

        // 预填充大于剩余空间时只写到满
        let (mut small, small_consumer) = RingBuffer::<f32>::new(4);
        let mut prefill = Prefill {
            samples: 6,
            pending: true,
        };
        assert!(prefill.ready(&mut small, true));
        assert_eq!(small_consumer.slots(), 4);
    }

    /// 闭环模拟共享模式的录音：音频引擎每 10 ms（QPC）交付一包，设备比 QPC 慢 30 ppm，攒不满一包时跳过这一次；
    /// 引擎每 1 ms 读 1 ms（先读后交付，取最坏的相位），控制器每秒按平均占用调整比率。
    /// 返回前 5 分钟之后，引擎读完时缓冲的最低占用（ms），小于 0 就是读空。
    fn lowest_level(target_ms: f64) -> f64 {
        const PERIOD_MS: u64 = 10;
        const SLOW_PPM: f64 = 30.0;
        const TOTAL_MS: u64 = 40 * 60_000;
        const STEADY_FROM_MS: u64 = 5 * 60_000;
        let mut controller = RateController::new(target_ms);
        let mut update = RateUpdate::default();
        let mut level = target_ms;
        // 设备已经录下、还没交付的数据：从半包开始，约 3 分钟后第一次跳包，之后每 333 秒一次
        let mut recorded_ms = PERIOD_MS as f64 / 2.0;
        let (mut sum, mut count) = (0.0, 0u32);
        let mut lowest = f64::MAX;
        for now in 1..=TOTAL_MS {
            recorded_ms += 1.0 - SLOW_PPM * 1e-6;
            level -= 1.0;
            if now >= STEADY_FROM_MS {
                lowest = lowest.min(level);
            }
            if now % PERIOD_MS == 0 && recorded_ms >= PERIOD_MS as f64 {
                recorded_ms -= PERIOD_MS as f64;
                sum += level;
                level += PERIOD_MS as f64 * relative_ratio(update.correction_ppm);
                sum += level;
                count += 2;
            }
            if now % 1000 == 0 {
                update = controller.update(sum / f64::from(count.max(1)), 1.0);
                (sum, count) = (0.0, 0);
            }
        }
        lowest
    }

    #[test]
    fn a_skipped_shared_packet_needs_the_margin() {
        let without = lowest_level(TARGET_LEVEL_MS);
        let with = lowest_level(TARGET_LEVEL_MS + SHARED_MARGIN_MS);
        assert!(without < 0.5, "the old target runs dry: {without} ms");
        assert!(with > 5.0, "the margin keeps a reserve: {with} ms");
    }
}
