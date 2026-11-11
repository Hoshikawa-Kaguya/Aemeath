//! 带单位的数值类型。反序列化时校验取值，非法值无法构造出来。

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 增益，单位 dB。
///
/// 范围只用来拒绝异常值（NaN、无穷大、明显错误的数值）；界面上的调节范围由前端决定。
/// 完全静音用通道条的静音或关闭连线表示，不用负无穷。
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "f32", into = "f32")]
pub struct GainDb(f32);

impl GainDb {
    pub const MIN: f32 = -120.0;
    pub const MAX: f32 = 24.0;
    pub const UNITY: Self = Self(0.0);

    pub fn new(db: f32) -> Result<Self, GainError> {
        if db.is_finite() && (Self::MIN..=Self::MAX).contains(&db) {
            Ok(Self(db))
        } else {
            Err(GainError(db))
        }
    }

    pub fn db(self) -> f32 {
        self.0
    }
}

impl TryFrom<f32> for GainDb {
    type Error = GainError;

    fn try_from(db: f32) -> Result<Self, Self::Error> {
        Self::new(db)
    }
}

impl From<GainDb> for f32 {
    fn from(gain: GainDb) -> Self {
        gain.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Error)]
#[error("gain {0} dB is not finite or outside {min}..={max} dB", min = GainDb::MIN, max = GainDb::MAX)]
pub struct GainError(pub f32);

/// 门限阈值，单位 dBFS（GLOSSARY：门限）：信号峰值低于它时关闭这一路。
///
/// 范围 −80 到 0 dBFS：再低就与本底噪声无法区分，高于 0 则永远不会打开。"关"用 `Option::None` 表示。
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "f32", into = "f32")]
pub struct GateThreshold(f32);

impl GateThreshold {
    pub const MIN: f32 = -80.0;
    pub const MAX: f32 = 0.0;

    pub fn new(db: f32) -> Result<Self, GateThresholdError> {
        if db.is_finite() && (Self::MIN..=Self::MAX).contains(&db) {
            Ok(Self(db))
        } else {
            Err(GateThresholdError(db))
        }
    }

    pub fn db(self) -> f32 {
        self.0
    }
}

impl TryFrom<f32> for GateThreshold {
    type Error = GateThresholdError;

    fn try_from(db: f32) -> Result<Self, Self::Error> {
        Self::new(db)
    }
}

impl From<GateThreshold> for f32 {
    fn from(threshold: GateThreshold) -> Self {
        threshold.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Error)]
#[error(
    "gate threshold {0} dBFS is not finite or outside {min}..={max} dBFS",
    min = GateThreshold::MIN,
    max = GateThreshold::MAX
)]
pub struct GateThresholdError(pub f32);

/// 独占模式的缓冲（设备周期），单位 ms（用户可以自定义）。
///
/// 范围只用来拒绝异常值：实际使用时还会被限制到设备的最小周期以上，并按设备要求对齐。
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "f32", into = "f32")]
pub struct BufferMs(f32);

impl BufferMs {
    pub const MIN: f32 = 1.0;
    pub const MAX: f32 = 100.0;

    pub fn new(ms: f32) -> Result<Self, BufferMsError> {
        if ms.is_finite() && (Self::MIN..=Self::MAX).contains(&ms) {
            Ok(Self(ms))
        } else {
            Err(BufferMsError(ms))
        }
    }

    pub fn ms(self) -> f32 {
        self.0
    }
}

impl TryFrom<f32> for BufferMs {
    type Error = BufferMsError;

    fn try_from(ms: f32) -> Result<Self, Self::Error> {
        Self::new(ms)
    }
}

impl From<BufferMs> for f32 {
    fn from(buffer: BufferMs) -> Self {
        buffer.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Error)]
#[error("buffer {0} ms is not finite or outside {min}..={max} ms", min = BufferMs::MIN, max = BufferMs::MAX)]
pub struct BufferMsError(pub f32);

/// 采样率，单位 Hz。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct SampleRate(u32);

impl SampleRate {
    pub const MIN_HZ: u32 = 8_000;
    /// 独占模式实测可到 768 kHz
    pub const MAX_HZ: u32 = 768_000;
    /// 最常用的采样率，用作各处的初始值
    pub const HZ_48000: Self = Self(48_000);

    pub fn new(hz: u32) -> Result<Self, SampleRateError> {
        if (Self::MIN_HZ..=Self::MAX_HZ).contains(&hz) {
            Ok(Self(hz))
        } else {
            Err(SampleRateError(hz))
        }
    }

    pub fn hz(self) -> u32 {
        self.0
    }
}

impl TryFrom<u32> for SampleRate {
    type Error = SampleRateError;

    fn try_from(hz: u32) -> Result<Self, Self::Error> {
        Self::new(hz)
    }
}

impl From<SampleRate> for u32 {
    fn from(rate: SampleRate) -> Self {
        rate.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("sample rate {0} Hz is outside {min}..={max} Hz", min = SampleRate::MIN_HZ, max = SampleRate::MAX_HZ)]
pub struct SampleRateError(pub u32);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_rejects_non_finite_and_out_of_range_values() {
        assert!(GainDb::new(0.0).is_ok());
        assert!(GainDb::new(GainDb::MIN).is_ok());
        assert!(GainDb::new(GainDb::MAX).is_ok());
        assert!(GainDb::new(f32::NAN).is_err());
        assert!(GainDb::new(f32::NEG_INFINITY).is_err());
        assert!(GainDb::new(GainDb::MAX + 1.0).is_err());
    }

    #[test]
    fn gain_is_validated_when_deserializing() {
        assert_eq!(serde_json::from_str::<GainDb>("-6.5").unwrap().db(), -6.5);
        assert!(serde_json::from_str::<GainDb>("1000").is_err());
    }

    #[test]
    fn gate_threshold_is_bounded() {
        assert!(GateThreshold::new(-40.0).is_ok());
        assert!(GateThreshold::new(GateThreshold::MIN - 1.0).is_err());
        assert!(GateThreshold::new(0.5).is_err());
        assert!(serde_json::from_str::<GateThreshold>("-90").is_err());
    }

    #[test]
    fn sample_rate_accepts_8k_to_768k() {
        assert!(SampleRate::new(7_999).is_err());
        assert!(SampleRate::new(48_000).is_ok());
        assert!(SampleRate::new(768_000).is_ok());
        assert!(SampleRate::new(768_001).is_err());
        assert!(serde_json::from_str::<SampleRate>("0").is_err());
    }
}
