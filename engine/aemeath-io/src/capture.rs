//! 录音流共用的一段：读包 → 转立体声 → ASRC → 环形缓冲 → 按水位调整比率。外部输入和应用源都用它。
//!
//! 重采样在生产方（IO 线程）进行，固定每次处理 1 ms 的设备帧；调度线程只按块取数据。
//! 控制器的反馈量是环形缓冲的水位：水位偏高说明设备时钟比引擎快，就把比率调低、少产出一些。

use aemeath_dsp::resample::{FixedSide, RateController, StereoResampler, relative_ratio};
use aemeath_protocol::topology::Resampler;
use aemeath_protocol::units::SampleRate;
use rtrb::Producer;
use wasapi::AudioCaptureClient;

use crate::bridge::{BridgeError, BridgeLog, CONTROL_PERIOD};
use crate::device::{FrameCodec, OpenStream};
use crate::{STEREO, TARGET_LEVEL_MS, frames_to_ms};

/// 每次重采样处理的设备帧数：1 ms
const CHUNKS_PER_SECOND: u32 = 1000;
/// 读设备数据的初始缓冲：100 ms，不够时再增长
const INITIAL_READ_MS: u32 = 100;

pub struct CapturePump {
    codec: FrameCodec,
    block_align: usize,
    engine_rate: SampleRate,
    resampler: StereoResampler,
    bytes: Vec<u8>,
    staging: Vec<f32>,
    produced: Vec<f32>,
    capacity: usize,
    controller: RateController,
    log: BridgeLog,
}

impl CapturePump {
    /// `label` 只用于日志（端点 ID 或程序路径）。
    pub fn new(
        stream: &OpenStream,
        engine_rate: SampleRate,
        quality: Resampler,
        producer: &Producer<f32>,
        label: &str,
    ) -> Result<Self, BridgeError> {
        let device_rate = stream.sample_rate();
        let chunk_frames = (device_rate.hz() / CHUNKS_PER_SECOND).max(1) as usize;
        let ratio = f64::from(engine_rate.hz()) / f64::from(device_rate.hz());
        let resampler = StereoResampler::new(quality, ratio, chunk_frames, FixedSide::Input)?;
        let bytes = vec![0u8; stream.block_align * (device_rate.hz() / (1000 / INITIAL_READ_MS)) as usize];
        let staging = Vec::with_capacity(bytes.len() / stream.block_align * STEREO);
        let produced = Vec::with_capacity(resampler.output_frames_max() * STEREO * 8);
        Ok(Self {
            codec: stream.frame_codec(),
            block_align: stream.block_align,
            engine_rate,
            resampler,
            bytes,
            staging,
            produced,
            capacity: producer.buffer().capacity(),
            controller: RateController::new(TARGET_LEVEL_MS),
            log: BridgeLog::new(label),
        })
    }

    /// 取走设备已交付的全部包，重采样后写进环形缓冲。
    pub fn read_packets(&mut self, capture: &AudioCaptureClient) -> Result<(), BridgeError> {
        while let Some(frames) = capture.get_next_packet_size()?.filter(|frames| *frames > 0) {
            let len = frames as usize * self.block_align;
            if self.bytes.len() < len {
                self.bytes.resize(len, 0);
            }
            let (read, info) = capture.read_from_device(&mut self.bytes[..len])?;
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

    /// 把攒够的数据重采样并写进环形缓冲，按水位调整比率。
    pub fn deliver(&mut self, producer: &mut Producer<f32>) -> Result<(), BridgeError> {
        let mut consumed = 0;
        loop {
            let needed = self.resampler.input_frames_next() * STEREO;
            if self.staging.len() - consumed < needed {
                break;
            }
            self.resampler
                .process(&self.staging[consumed..consumed + needed], &mut self.produced)?;
            consumed += needed;
        }
        self.staging.drain(..consumed);

        let writable = producer.slots().min(self.produced.len());
        if let Ok(chunk) = producer.write_chunk_uninit(writable) {
            chunk.fill_from_iter(self.produced.iter().copied());
        }
        let dropped = self.produced.len() - writable;
        if dropped > 0 {
            self.log.overflow(dropped / STEREO);
        }
        self.produced.clear();

        let level_ms = frames_to_ms((self.capacity - producer.slots()) / STEREO, self.engine_rate);
        self.log.level(level_ms);
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
}
