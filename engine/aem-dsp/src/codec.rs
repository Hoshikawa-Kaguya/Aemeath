//! 样本格式与 f32 的互相转换（引擎内部统一用 f32，选型报告 6.3 节）。
//!
//! 整数按 2 的幂缩放（例如 16 位除以 32768），再按同样的比例取整编码回去，
//! 所以 16 位和 24 位整数经过"解码 → 编码"可以逐位还原，直通时没有损失。
//! 超出 [-1, 1) 的值在编码时削波。24 位放在 32 位容器里的格式与 32 位整数一样左对齐。

use aem_protocol::message::SampleFormat;

const SCALE_16: f32 = 32_768.0;
const SCALE_24: f32 = 8_388_608.0;
const SCALE_32: f32 = 2_147_483_648.0;
const INT24_MIN: i32 = -(1 << 23);
const INT24_MAX: i32 = (1 << 23) - 1;
/// 24 位样本放在 32 位容器里时左移的位数
const CONTAINER_SHIFT: u32 = 8;

/// 每个样本占用的字节数。
pub fn bytes_per_sample(format: SampleFormat) -> usize {
    match format {
        SampleFormat::Int16 => 2,
        SampleFormat::Int24 => 3,
        SampleFormat::Int24In32 | SampleFormat::Int32 | SampleFormat::Float32 => 4,
        SampleFormat::Float64 => 8,
    }
}

/// 把 `bytes` 解码追加到 `out`（交错排列，小端）。不完整的最后一个样本被忽略。
pub fn decode(format: SampleFormat, bytes: &[u8], out: &mut Vec<f32>) {
    let size = bytes_per_sample(format);
    out.extend(bytes.chunks_exact(size).map(|sample| decode_one(format, sample)));
}

fn decode_one(format: SampleFormat, sample: &[u8]) -> f32 {
    match format {
        SampleFormat::Int16 => f32::from(i16::from_le_bytes([sample[0], sample[1]])) / SCALE_16,
        SampleFormat::Int24 => {
            // 放进 i32 的高 24 位再算术右移，得到带符号的 24 位值
            let value = i32::from_le_bytes([0, sample[0], sample[1], sample[2]]) >> CONTAINER_SHIFT;
            value as f32 / SCALE_24
        }
        SampleFormat::Int24In32 | SampleFormat::Int32 => {
            i32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]) as f32 / SCALE_32
        }
        SampleFormat::Float32 => f32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]),
        SampleFormat::Float64 => f64::from_le_bytes([
            sample[0], sample[1], sample[2], sample[3], sample[4], sample[5], sample[6], sample[7],
        ]) as f32,
    }
}

/// 把 `samples` 编码追加到 `out`（交错排列，小端）。
pub fn encode(format: SampleFormat, samples: &[f32], out: &mut Vec<u8>) {
    out.reserve(samples.len() * bytes_per_sample(format));
    for &sample in samples {
        match format {
            SampleFormat::Int16 => out.extend_from_slice(&quantize(sample, SCALE_16, i16::MIN, i16::MAX).to_le_bytes()),
            SampleFormat::Int24 => {
                let value = quantize(sample, SCALE_24, INT24_MIN, INT24_MAX);
                out.extend_from_slice(&value.to_le_bytes()[..3]);
            }
            SampleFormat::Int24In32 => {
                let value = quantize(sample, SCALE_24, INT24_MIN, INT24_MAX) << CONTAINER_SHIFT;
                out.extend_from_slice(&value.to_le_bytes());
            }
            SampleFormat::Int32 => out.extend_from_slice(&quantize(sample, SCALE_32, i32::MIN, i32::MAX).to_le_bytes()),
            SampleFormat::Float32 => out.extend_from_slice(&sample.to_le_bytes()),
            SampleFormat::Float64 => out.extend_from_slice(&f64::from(sample).to_le_bytes()),
        }
    }
}

/// 缩放、四舍五入并限制在整数范围内。NaN 编码为 0。
fn quantize<T>(sample: f32, scale: f32, min: T, max: T) -> T
where
    T: Copy + Into<f64> + FromF64,
{
    if sample.is_nan() {
        return T::from_f64(0.0);
    }
    let scaled = (f64::from(sample) * f64::from(scale)).round();
    T::from_f64(scaled.clamp(min.into(), max.into()))
}

/// 已限制在范围内的 f64 转为整数。
trait FromF64 {
    fn from_f64(value: f64) -> Self;
}

impl FromF64 for i16 {
    fn from_f64(value: f64) -> Self {
        value as i16
    }
}

impl FromF64 for i32 {
    fn from_f64(value: f64) -> Self {
        value as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_FORMATS: [SampleFormat; 6] = [
        SampleFormat::Int16,
        SampleFormat::Int24,
        SampleFormat::Int24In32,
        SampleFormat::Int32,
        SampleFormat::Float32,
        SampleFormat::Float64,
    ];

    fn round_trip(format: SampleFormat, bytes: &[u8]) -> Vec<u8> {
        let mut samples = Vec::new();
        decode(format, bytes, &mut samples);
        let mut encoded = Vec::new();
        encode(format, &samples, &mut encoded);
        encoded
    }

    #[test]
    fn int16_and_int24_pass_through_bit_exactly() {
        let int16: Vec<u8> = [i16::MIN, -1, 0, 1, 12_345, i16::MAX]
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect();
        assert_eq!(round_trip(SampleFormat::Int16, &int16), int16);

        let int24: Vec<u8> = [-(1i32 << 23), -1, 0, 1, 4_567_890, (1 << 23) - 1]
            .iter()
            .flat_map(|value| value.to_le_bytes()[..3].to_vec())
            .collect();
        assert_eq!(round_trip(SampleFormat::Int24, &int24), int24);

        let int24_in_32: Vec<u8> = [-(1i32 << 23), -7, 0, 7, (1 << 23) - 1]
            .iter()
            .flat_map(|value| (value << 8).to_le_bytes())
            .collect();
        assert_eq!(round_trip(SampleFormat::Int24In32, &int24_in_32), int24_in_32);
    }

    #[test]
    fn decodes_full_scale_to_minus_one() {
        let mut out = Vec::new();
        decode(SampleFormat::Int16, &i16::MIN.to_le_bytes(), &mut out);
        decode(SampleFormat::Int24, &[0x00, 0x00, 0x80], &mut out);
        decode(SampleFormat::Int32, &i32::MIN.to_le_bytes(), &mut out);
        assert_eq!(out, vec![-1.0, -1.0, -1.0]);
    }

    #[test]
    fn clips_out_of_range_values_and_maps_nan_to_zero() {
        let mut out = Vec::new();
        encode(SampleFormat::Int16, &[2.0, -2.0, f32::NAN], &mut out);
        let values: Vec<i16> = out
            .as_chunks::<2>()
            .0
            .iter()
            .map(|bytes| i16::from_le_bytes(*bytes))
            .collect();
        assert_eq!(values, vec![i16::MAX, i16::MIN, 0]);
    }

    #[test]
    fn every_format_keeps_half_scale_within_its_resolution() {
        for format in ALL_FORMATS {
            let mut bytes = Vec::new();
            encode(format, &[0.5, -0.25], &mut bytes);
            assert_eq!(bytes.len(), 2 * bytes_per_sample(format));
            let mut decoded = Vec::new();
            decode(format, &bytes, &mut decoded);
            assert!((decoded[0] - 0.5).abs() < 1e-4, "{format:?}: {decoded:?}");
            assert!((decoded[1] + 0.25).abs() < 1e-4, "{format:?}: {decoded:?}");
        }
    }

    #[test]
    fn ignores_a_trailing_partial_sample() {
        let mut out = Vec::new();
        decode(SampleFormat::Int16, &[0, 0, 1], &mut out);
        assert_eq!(out.len(), 1);
    }
}
