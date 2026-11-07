//! 重采样（rubato，S5）：4 档算法，比率可在运行时微调，用于异步采样率转换（ASRC）和固定比率转换。
//!
//! 只处理交错立体声 f32。构建时分配内存；[`StereoResampler::process`] 不分配内存
//! （输出写进调用方预留好容量的 `Vec`，容量不够时才会增长）。

use aemeath_protocol::topology::Resampler as Quality;
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, FixedAsync, PolynomialDegree, Resampler, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use thiserror::Error;

use crate::STEREO;

/// 比率可调范围 ±1%，远大于 ±1000 ppm 的设计要求，给控制器的瞬时调整留出余量（S5）
pub const MAX_RELATIVE_RATIO: f64 = 1.01;
const SINC_HIGH_TAPS: usize = 256;
const SINC_HIGH_OVERSAMPLING: usize = 256;
const SINC_STANDARD_TAPS: usize = 128;
const SINC_STANDARD_OVERSAMPLING: usize = 128;

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

impl StereoResampler {
    /// `ratio` = 输出采样率 / 输入采样率；`chunk_frames` 是固定一侧每块的帧数。
    pub fn new(quality: Quality, ratio: f64, chunk_frames: usize, fixed: FixedSide) -> Result<Self, ResampleError> {
        let fixed = match fixed {
            FixedSide::Input => FixedAsync::Input,
            FixedSide::Output => FixedAsync::Output,
        };
        let inner: Box<dyn Resampler<f32>> = match quality {
            Quality::SincHigh => {
                let parameters = SincInterpolationParameters::new(SINC_HIGH_TAPS, WindowFunction::BlackmanHarris2)
                    .oversampling_factor(SINC_HIGH_OVERSAMPLING)
                    .interpolation(SincInterpolationType::Cubic);
                Box::new(Async::<f32>::new_sinc(
                    ratio,
                    MAX_RELATIVE_RATIO,
                    &parameters,
                    chunk_frames,
                    STEREO,
                    fixed,
                )?)
            }
            Quality::SincStandard => {
                let parameters = SincInterpolationParameters::new(SINC_STANDARD_TAPS, WindowFunction::Blackman2)
                    .oversampling_factor(SINC_STANDARD_OVERSAMPLING)
                    .interpolation(SincInterpolationType::Cubic);
                Box::new(Async::<f32>::new_sinc(
                    ratio,
                    MAX_RELATIVE_RATIO,
                    &parameters,
                    chunk_frames,
                    STEREO,
                    fixed,
                )?)
            }
            Quality::Polynomial => Box::new(Async::<f32>::new_poly(
                ratio,
                MAX_RELATIVE_RATIO,
                PolynomialDegree::Quintic,
                chunk_frames,
                STEREO,
                fixed,
            )?),
            Quality::Linear => Box::new(Async::<f32>::new_poly(
                ratio,
                MAX_RELATIVE_RATIO,
                PolynomialDegree::Linear,
                chunk_frames,
                STEREO,
                fixed,
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
    fn converts_44k1_to_48k_without_changing_pitch() {
        for quality in [
            Quality::SincHigh,
            Quality::SincStandard,
            Quality::Polynomial,
            Quality::Linear,
        ] {
            let mut resampler = StereoResampler::new(quality, 48_000.0 / 44_100.0, CHUNK, FixedSide::Input).unwrap();
            let input = sine(44_100.0, 1_000.0, CHUNK * 100);
            let mut output = Vec::new();
            for chunk in input.chunks(CHUNK * STEREO) {
                resampler.process(chunk, &mut output).unwrap();
            }
            let produced = output.len() / STEREO;
            // 输出帧数约为输入的 48/44.1
            let expected = (CHUNK * 100) as f64 * 48_000.0 / 44_100.0;
            assert!(
                (produced as f64 - expected).abs() < 2.0 * CHUNK as f64,
                "{quality:?}: {produced}"
            );
            let skip = resampler.output_delay_frames() * STEREO;
            let measured = frequency(&output[skip..], 48_000.0);
            assert!((measured - 1_000.0).abs() < 5.0, "{quality:?}: {measured} Hz");
        }
    }

    #[test]
    fn fixed_output_side_always_produces_the_chunk() {
        let mut resampler =
            StereoResampler::new(Quality::SincStandard, 48_000.0 / 44_100.0, CHUNK, FixedSide::Output).unwrap();
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
