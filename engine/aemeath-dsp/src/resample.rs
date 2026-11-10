//! 重采样（rubato，S5）：三种算法，各带固定的重采样预设，用于异步采样率转换（ASRC）和固定比率转换。
//!
//! 只处理交错立体声 f32。构建时分配内存；`process` 不分配内存
//! （输出写进调用方预留好容量的 `Vec`，容量不够时才会增长）。

use aemeath_protocol::topology::{FftPreset, PolyPreset, Resampler as Quality, SincPreset};
use aemeath_protocol::units::SampleRate;
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, Fft, FixedAsync, FixedSync, PolynomialDegree, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};
use thiserror::Error;

use crate::STEREO;

/// 比率可调范围 ±1%，远大于 ±1000 ppm 的设计要求，给控制器的瞬时调整留出余量（S5）
pub const MAX_RELATIVE_RATIO: f64 = 1.01;
/// FFT 预设的帧数以这个采样率为基准，按实际采样率换算，各采样率下延迟一致
const FFT_REFERENCE_RATE: usize = 48_000;
const FFT_LOW_LATENCY_FRAMES: usize = 256;
const FFT_BALANCED_FRAMES: usize = 1024;
/// 整块做一次 FFT：分块变大时滤波器随之变长，通带更宽。切成子块则分块大小只增加延迟（M4 计划 1.1 节实测）
const FFT_SUB_CHUNKS: usize = 1;
/// FFT 路径上吸收时钟漂移的小比率级（R7），放在引擎采样率一侧，用户不可选
const DRIFT_STAGE: Quality = Quality::SincAsync {
    preset: SincPreset::Fast,
};
/// 小比率级每块的时长：1 ms
const DRIFT_CHUNKS_PER_SECOND: u32 = 1000;
const MILLISECONDS_PER_SECOND: f64 = 1000.0;

fn sinc_parameters(preset: SincPreset) -> SincInterpolationParameters {
    use SincInterpolationType::{Cubic, Linear, Quadratic};
    use WindowFunction::{Blackman2, BlackmanHarris2, Hann2};
    let (sinc_len, oversampling, interpolation, window) = match preset {
        SincPreset::VeryFast => (64, 1024, Linear, Hann2),
        SincPreset::Fast => (128, 1024, Linear, Blackman2),
        SincPreset::Balanced => (192, 512, Quadratic, BlackmanHarris2),
        SincPreset::Accurate => (256, 256, Cubic, BlackmanHarris2),
        SincPreset::Ultra => (512, 256, Cubic, BlackmanHarris2),
    };
    SincInterpolationParameters::new(sinc_len, window)
        .oversampling_factor(oversampling)
        .interpolation(interpolation)
}

fn poly_degree(preset: PolyPreset) -> PolynomialDegree {
    match preset {
        PolyPreset::Fastest => PolynomialDegree::Linear,
        PolyPreset::Smooth => PolynomialDegree::Cubic,
        PolyPreset::Fine => PolynomialDegree::Quintic,
    }
}

/// FFT 的输入分块。取比率允许的最小块的整数倍：否则 rubato 把内部 FFT 块取整得比分块大，
/// 有的调用没有产出，两批之间的间隔会达到两个分块。
fn fft_chunk_frames(preset: FftPreset, input_rate: SampleRate, output_rate: SampleRate) -> usize {
    let frames = match preset {
        FftPreset::LowLatency => FFT_LOW_LATENCY_FRAMES,
        FftPreset::Balanced => FFT_BALANCED_FRAMES,
    };
    let scaled = (frames * input_rate.hz() as usize / FFT_REFERENCE_RATE).max(1);
    let smallest_block = input_rate.hz() as usize / greatest_common_divisor(input_rate.hz(), output_rate.hz()) as usize;
    scaled.div_ceil(smallest_block) * smallest_block
}

fn greatest_common_divisor(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { greatest_common_divisor(b, a % b) }
}

#[derive(Debug, Error)]
pub enum ResampleError {
    #[error("cannot create the resampler: {0}")]
    Construction(#[from] rubato::ResamplerConstructionError),
    #[error("resampling failed: {0}")]
    Process(#[from] rubato::ResampleError),
    #[error("invalid buffer: {0}")]
    Buffer(String),
}

/// 哪一侧的块大小固定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixedSide {
    /// 每次处理固定帧数的输入（生产方做重采样时用，例如外部输入设备的 IO 线程）
    Input,
    /// 每次产出固定帧数的输出（消费方做重采样时用，例如外部输出设备的 IO 线程）
    Output,
}

pub struct StereoResampler {
    inner: Box<dyn Resampler<f32>>,
    scratch: Vec<f32>,
}

impl std::fmt::Debug for StereoResampler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StereoResampler")
    }
}

impl StereoResampler {
    /// `chunk_frames` 是固定一侧每块的帧数；FFT 的分块由预设决定，忽略这个参数。
    pub fn new(
        quality: Quality,
        input_rate: SampleRate,
        output_rate: SampleRate,
        chunk_frames: usize,
        fixed: FixedSide,
    ) -> Result<Self, ResampleError> {
        let ratio = f64::from(output_rate.hz()) / f64::from(input_rate.hz());
        let fixed_async = match fixed {
            FixedSide::Input => FixedAsync::Input,
            FixedSide::Output => FixedAsync::Output,
        };
        let inner: Box<dyn Resampler<f32>> = match quality {
            Quality::PolyAsync { preset } => Box::new(Async::<f32>::new_poly(
                ratio,
                MAX_RELATIVE_RATIO,
                poly_degree(preset),
                chunk_frames,
                STEREO,
                fixed_async,
            )?),
            Quality::SincAsync { preset } => Box::new(Async::<f32>::new_sinc(
                ratio,
                MAX_RELATIVE_RATIO,
                &sinc_parameters(preset),
                chunk_frames,
                STEREO,
                fixed_async,
            )?),
            Quality::FftSync { preset } => Box::new(Fft::<f32>::new_custom(
                input_rate.hz() as usize,
                output_rate.hz() as usize,
                fft_chunk_frames(preset, input_rate, output_rate),
                FFT_SUB_CHUNKS,
                STEREO,
                WindowFunction::BlackmanHarris2,
                match fixed {
                    FixedSide::Input => FixedSync::Input,
                    FixedSide::Output => FixedSync::Output,
                },
            )?),
        };
        let scratch = vec![0.0; inner.output_frames_max() * STEREO];
        Ok(Self { inner, scratch })
    }

    /// 下一次 [`Self::process`] 需要的输入帧数
    pub fn input_frames_next(&self) -> usize {
        self.inner.input_frames_next()
    }

    /// 一次处理最多产出的帧数
    pub fn output_frames_max(&self) -> usize {
        self.inner.output_frames_max()
    }

    /// 重采样引入的延迟（输出帧数）
    pub fn output_delay_frames(&self) -> usize {
        self.inner.output_delay()
    }

    /// 相对于构建时比率的微调（1.0 为不调整）。带平滑过渡，不会产生跳变。
    pub fn set_relative_ratio(&mut self, relative: f64) -> Result<(), ResampleError> {
        if let Some(adjustable) = self.inner.as_adjustable() {
            adjustable.set_resample_ratio_relative(relative, true)?;
        }
        Ok(())
    }

    /// 处理正好 [`Self::input_frames_next`] 帧的交错立体声输入，把结果追加到 `out`，返回产出的帧数。
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<usize, ResampleError> {
        let frames_in = input.len() / STEREO;
        let frames_out = self.scratch.len() / STEREO;
        let input = InterleavedSlice::new(input, STEREO, frames_in)
            .map_err(|error| ResampleError::Buffer(error.to_string()))?;
        let mut output = InterleavedSlice::new_mut(&mut self.scratch, STEREO, frames_out)
            .map_err(|error| ResampleError::Buffer(error.to_string()))?;
        let (_, produced) = self.inner.process_into_buffer(&input, &mut output, None)?;
        out.extend_from_slice(&self.scratch[..produced * STEREO]);
        Ok(produced)
    }
}

/// 外部设备和应用源的桥接重采样：转换采样率并吸收时钟漂移。
///
/// Poly、Sinc 一级完成。FFT 只能做固定比率，采样率不同时分两级：FFT 转换采样率，
/// 小比率级（[`DRIFT_STAGE`]）在引擎采样率一侧吸收漂移（R7）；采样率相同时跳过 FFT。
pub struct BridgeResampler {
    first: StereoResampler,
    second: Option<StereoResampler>,
    /// 两级之间尚未处理的样本，都在引擎采样率上
    pending: Vec<f32>,
    burst_ms: f64,
}

impl BridgeResampler {
    /// `fixed`：`Input` 用于录音方向（输出侧是引擎），`Output` 用于播放方向（输入侧是引擎）。
    pub fn new(
        quality: Quality,
        input_rate: SampleRate,
        output_rate: SampleRate,
        chunk_frames: usize,
        fixed: FixedSide,
    ) -> Result<Self, ResampleError> {
        if !matches!(quality, Quality::FftSync { .. }) {
            return Ok(Self::single(StereoResampler::new(
                quality,
                input_rate,
                output_rate,
                chunk_frames,
                fixed,
            )?));
        }
        if input_rate == output_rate {
            return Ok(Self::single(StereoResampler::new(
                DRIFT_STAGE,
                input_rate,
                output_rate,
                chunk_frames,
                fixed,
            )?));
        }
        let fft = StereoResampler::new(quality, input_rate, output_rate, chunk_frames, FixedSide::Input)?;
        // 按最大单次产出算：rubato 把 FFT 分块取整后，一批可能比输入分块对应的帧数多
        let burst_ms = fft.output_frames_max() as f64 * MILLISECONDS_PER_SECOND / f64::from(output_rate.hz());
        let engine_rate = match fixed {
            FixedSide::Input => output_rate,
            FixedSide::Output => input_rate,
        };
        let drift_frames = (engine_rate.hz() / DRIFT_CHUNKS_PER_SECOND).max(1) as usize;
        let drift = StereoResampler::new(DRIFT_STAGE, engine_rate, engine_rate, drift_frames, FixedSide::Input)?;
        let (first, second) = match fixed {
            FixedSide::Input => (fft, drift),
            FixedSide::Output => (drift, fft),
        };
        let pending = Vec::with_capacity((first.output_frames_max() + second.input_frames_next()) * STEREO * 2);
        Ok(Self {
            first,
            second: Some(second),
            pending,
            burst_ms,
        })
    }

    fn single(first: StereoResampler) -> Self {
        Self {
            first,
            second: None,
            pending: Vec::new(),
            burst_ms: 0.0,
        }
    }

    /// 下一次 [`Self::process`] 需要的输入帧数
    pub fn input_frames_next(&self) -> usize {
        self.first.input_frames_next()
    }

    /// 一次处理最多产出的帧数
    pub fn output_frames_max(&self) -> usize {
        let second = self.second.as_ref().map_or(0, StereoResampler::output_frames_max);
        self.first.output_frames_max().max(second) + second
    }

    /// 成团量：产出按这个时长成批到达（ms）。一级完成时为 0。
    pub fn burst_ms(&self) -> f64 {
        self.burst_ms
    }

    /// 两级之间还没处理的帧数（引擎采样率）
    pub fn pending_frames(&self) -> usize {
        self.pending.len() / STEREO
    }

    /// 相对于构建时比率的微调（1.0 为不调整）。FFT 一级不支持微调，由小比率级承担。
    pub fn set_relative_ratio(&mut self, relative: f64) -> Result<(), ResampleError> {
        self.first.set_relative_ratio(relative)?;
        if let Some(second) = &mut self.second {
            second.set_relative_ratio(relative)?;
        }
        Ok(())
    }

    /// 处理正好 [`Self::input_frames_next`] 帧的输入，把结果追加到 `out`，返回产出的帧数（FFT 路径上可能为 0）。
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<usize, ResampleError> {
        let Some(second) = &mut self.second else {
            return self.first.process(input, out);
        };
        self.first.process(input, &mut self.pending)?;
        let mut consumed = 0;
        let mut produced = 0;
        loop {
            let needed = second.input_frames_next() * STEREO;
            if self.pending.len() - consumed < needed {
                break;
            }
            produced += second.process(&self.pending[consumed..consumed + needed], out)?;
            consumed += needed;
        }
        self.pending.drain(..consumed);
        Ok(produced)
    }
}

/// 时钟漂移的 PI 控制器（S5）。
///
/// 反馈量是桥接缓冲的水位（ms）；输出是比率校正量（ppm）。校正量为正表示"消耗输入更快"，即把输出/输入比率调低。
///
/// 连续时间模型：水位误差 e 的变化率 de/dt = (d − c)·1e-3 ms/s，d 为两时钟的相对漂移（ppm），c 为校正量。
/// 取 c = Kp·e + Ki·∫e dt。默认 Kp = 40 ppm/ms、Ki = 1 ppm/(ms·s)：自然频率约 0.032 rad/s、阻尼比约 0.63，
/// 时间常数约 1 分钟，校正量不会因水位的毫秒级抖动而大幅跳动。
///
/// 条件积分：误差超过 [`INTEGRATE_WITHIN_MS`] 时视为突发事件（例如其他程序改变了设备的引擎周期，
/// 数据短暂中断、水位阶跃），只让比例项负责回填，不累积积分（S5 实测）。
#[derive(Debug, Clone)]
pub struct RateController {
    target_ms: f64,
    integral: f64,
}

pub const DEFAULT_KP_PPM_PER_MS: f64 = 40.0;
pub const DEFAULT_KI_PPM_PER_MS_S: f64 = 1.0;
/// 校正量上限（选型报告 6.3 节：比率调节范围按 ±1000 ppm 设计）
pub const MAX_CORRECTION_PPM: f64 = 1000.0;
/// 只有水位误差在这个范围内才累积积分（ms）
pub const INTEGRATE_WITHIN_MS: f64 = 3.0;
const PPM: f64 = 1e-6;

/// 一次更新的结果，供日志使用。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RateUpdate {
    pub error_ms: f64,
    pub proportional_ppm: f64,
    /// 积分项：稳态时就是两台设备时钟的相对漂移估计
    pub integral_ppm: f64,
    pub correction_ppm: f64,
}

impl RateController {
    pub fn new(target_ms: f64) -> Self {
        Self {
            target_ms,
            integral: 0.0,
        }
    }

    /// `average_level_ms`：上一个调整周期内的平均水位；`period_s`：调整周期（秒）。
    pub fn update(&mut self, average_level_ms: f64, period_s: f64) -> RateUpdate {
        let error_ms = average_level_ms - self.target_ms;
        if error_ms.abs() <= INTEGRATE_WITHIN_MS {
            self.integral += error_ms * period_s;
        }
        // 抗积分饱和：积分项单独不得超过校正上限
        let limit = MAX_CORRECTION_PPM / DEFAULT_KI_PPM_PER_MS_S;
        self.integral = self.integral.clamp(-limit, limit);
        let proportional_ppm = DEFAULT_KP_PPM_PER_MS * error_ms;
        let integral_ppm = DEFAULT_KI_PPM_PER_MS_S * self.integral;
        let correction_ppm = (proportional_ppm + integral_ppm).clamp(-MAX_CORRECTION_PPM, MAX_CORRECTION_PPM);
        RateUpdate {
            error_ms,
            proportional_ppm,
            integral_ppm,
            correction_ppm,
        }
    }
}

/// 把校正量换算成相对比率（输出/输入），校正为正时比率变小。
pub fn relative_ratio(correction_ppm: f64) -> f64 {
    1.0 / (1.0 + correction_ppm * PPM)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    const CHUNK: usize = 480;
    const ALL_PRESETS: [Quality; 10] = [
        Quality::PolyAsync {
            preset: PolyPreset::Fastest,
        },
        Quality::PolyAsync {
            preset: PolyPreset::Smooth,
        },
        Quality::PolyAsync {
            preset: PolyPreset::Fine,
        },
        Quality::SincAsync {
            preset: SincPreset::VeryFast,
        },
        Quality::SincAsync {
            preset: SincPreset::Fast,
        },
        Quality::SincAsync {
            preset: SincPreset::Balanced,
        },
        Quality::SincAsync {
            preset: SincPreset::Accurate,
        },
        Quality::SincAsync {
            preset: SincPreset::Ultra,
        },
        Quality::FftSync {
            preset: FftPreset::LowLatency,
        },
        Quality::FftSync {
            preset: FftPreset::Balanced,
        },
    ];
    const FFT_BALANCED: Quality = Quality::FftSync {
        preset: FftPreset::Balanced,
    };

    fn rate(hz: u32) -> SampleRate {
        SampleRate::new(hz).unwrap()
    }

    /// 把输入按重采样器要的块大小依次送入，返回全部产出和消耗的输入帧数
    fn run_bridge(resampler: &mut BridgeResampler, input: &[f32]) -> (Vec<f32>, usize) {
        let mut position = 0;
        let mut output = Vec::new();
        loop {
            let needed = resampler.input_frames_next() * STEREO;
            if position + needed > input.len() {
                return (output, position / STEREO);
            }
            resampler
                .process(&input[position..position + needed], &mut output)
                .unwrap();
            position += needed;
        }
    }

    fn sine(rate: f64, frequency: f64, frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|index| {
                let value = (0.5 * (TAU * frequency * index as f64 / rate).sin()) as f32;
                [value, value]
            })
            .collect()
    }

    /// 统计左声道的上升过零次数，估计频率
    fn frequency(samples: &[f32], rate: f64) -> f64 {
        let left: Vec<f32> = samples.iter().step_by(STEREO).copied().collect();
        let crossings = left.windows(2).filter(|pair| pair[0] < 0.0 && pair[1] >= 0.0).count();
        crossings as f64 * rate / left.len() as f64
    }

    #[test]
    fn every_preset_converts_44k1_to_48k_without_changing_pitch() {
        for quality in ALL_PRESETS {
            for fixed in [FixedSide::Input, FixedSide::Output] {
                let mut resampler = BridgeResampler::new(quality, rate(44_100), rate(48_000), CHUNK, fixed).unwrap();
                let input = sine(44_100.0, 1_000.0, 48_000);
                let (output, consumed) = run_bridge(&mut resampler, &input);
                let produced = output.len() / STEREO;
                // 输出帧数约为输入的 48/44.1，两级路径上最多差一个分块
                let expected = consumed as f64 * 48_000.0 / 44_100.0;
                let margin = 3.0 * resampler.input_frames_next().max(CHUNK) as f64 + 48.0 * resampler.burst_ms();
                assert!((produced as f64 - expected).abs() < margin, "{quality:?}: {produced}");
                // 跳过启动时的滤波器延迟再测频率
                let skip = 4_800 * STEREO;
                let measured = frequency(&output[skip..], 48_000.0);
                assert!((measured - 1_000.0).abs() < 5.0, "{quality:?} {fixed:?}: {measured} Hz");
            }
        }
    }

    #[test]
    fn fft_converts_in_two_stages_and_reports_its_burst() {
        for fixed in [FixedSide::Input, FixedSide::Output] {
            let resampler = BridgeResampler::new(FFT_BALANCED, rate(48_000), rate(192_000), CHUNK, fixed).unwrap();
            assert!(resampler.second.is_some());
            assert!((resampler.burst_ms() - 21.33).abs() < 0.1, "{}", resampler.burst_ms());
        }
    }

    #[test]
    fn fft_chunk_scales_with_the_input_rate() {
        assert_eq!(
            fft_chunk_frames(FftPreset::LowLatency, rate(48_000), rate(192_000)),
            256
        );
        assert_eq!(fft_chunk_frames(FftPreset::LowLatency, rate(96_000), rate(48_000)), 512);
        assert_eq!(fft_chunk_frames(FftPreset::Balanced, rate(192_000), rate(48_000)), 4096);
    }

    #[test]
    fn fft_chunk_is_a_whole_number_of_ratio_blocks() {
        // 44.1k→48k 的最小块是 147 帧，48k→44.1k 是 160 帧
        assert_eq!(fft_chunk_frames(FftPreset::LowLatency, rate(44_100), rate(48_000)), 294);
        assert_eq!(fft_chunk_frames(FftPreset::Balanced, rate(48_000), rate(44_100)), 1120);
    }

    #[test]
    fn every_fft_call_produces_output_when_the_rates_are_not_multiples() {
        for (input, output) in [(44_100, 48_000), (48_000, 44_100)] {
            let mut resampler =
                StereoResampler::new(FFT_BALANCED, rate(input), rate(output), CHUNK, FixedSide::Input).unwrap();
            let block = vec![0.0; resampler.input_frames_next() * STEREO];
            let mut out = Vec::new();
            for _ in 0..8 {
                assert!(resampler.process(&block, &mut out).unwrap() > 0, "{input} -> {output}");
            }
        }
    }

    #[test]
    fn fft_is_skipped_when_the_rates_match() {
        let resampler = BridgeResampler::new(FFT_BALANCED, rate(48_000), rate(48_000), 48, FixedSide::Input).unwrap();
        assert!(resampler.second.is_none());
        assert_eq!(resampler.burst_ms(), 0.0);
    }

    #[test]
    fn fft_path_follows_the_drift_correction() {
        let mut resampler =
            BridgeResampler::new(FFT_BALANCED, rate(44_100), rate(48_000), CHUNK, FixedSide::Input).unwrap();
        let input = sine(44_100.0, 1_000.0, 88_200);
        let (nominal, _) = run_bridge(&mut resampler, &input);
        let mut corrected =
            BridgeResampler::new(FFT_BALANCED, rate(44_100), rate(48_000), CHUNK, FixedSide::Input).unwrap();
        corrected.set_relative_ratio(relative_ratio(1000.0)).unwrap();
        let (slower, _) = run_bridge(&mut corrected, &input);
        // +1000 ppm 的校正少产出约 0.1%（2 秒约 96 帧）
        let difference = (nominal.len() - slower.len()) / STEREO;
        assert!((40..=160).contains(&difference), "{difference}");
    }

    #[test]
    fn fixed_output_side_always_produces_the_chunk() {
        let quality = Quality::default();
        let mut resampler =
            StereoResampler::new(quality, rate(44_100), rate(48_000), CHUNK, FixedSide::Output).unwrap();
        let input = sine(44_100.0, 440.0, 10_000);
        let mut position = 0;
        let mut output = Vec::new();
        for _ in 0..10 {
            let needed = resampler.input_frames_next() * STEREO;
            let produced = resampler
                .process(&input[position..position + needed], &mut output)
                .unwrap();
            assert_eq!(produced, CHUNK);
            position += needed;
        }
    }

    #[test]
    fn controller_converges_to_the_drift() {
        // 模拟：两台时钟相差 +50 ppm，水位按 de/dt = (d − c)·1e-3 变化
        let drift_ppm = 50.0;
        let mut controller = RateController::new(15.0);
        let mut level = 15.0;
        let mut update = RateUpdate::default();
        for _ in 0..600 {
            level += (drift_ppm - update.correction_ppm) * 1e-3;
            update = controller.update(level, 1.0);
        }
        assert!((update.integral_ppm - drift_ppm).abs() < 1.0, "{update:?}");
        assert!(update.error_ms.abs() < 0.1, "{update:?}");
    }

    #[test]
    fn controller_does_not_integrate_large_steps() {
        let mut controller = RateController::new(15.0);
        let update = controller.update(25.0, 1.0);
        assert_eq!(update.integral_ppm, 0.0);
        assert_eq!(update.proportional_ppm, 400.0);
    }

    #[test]
    fn positive_correction_lowers_the_ratio() {
        assert!(relative_ratio(100.0) < 1.0);
        assert!((relative_ratio(0.0) - 1.0).abs() < f64::EPSILON);
    }
}
