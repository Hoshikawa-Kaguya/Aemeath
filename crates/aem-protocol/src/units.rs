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

/// 采样率，单位 Hz。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct SampleRate(u32);

impl SampleRate {
    pub const MIN_HZ: u32 = 8_000;
    /// 独占模式实测可到 768 kHz（选型报告 6.3 节）
    pub const MAX_HZ: u32 = 768_000;

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
    fn sample_rate_accepts_8k_to_768k() {
        assert!(SampleRate::new(7_999).is_err());
        assert!(SampleRate::new(48_000).is_ok());
        assert!(SampleRate::new(768_000).is_ok());
        assert!(SampleRate::new(768_001).is_err());
        assert!(serde_json::from_str::<SampleRate>("0").is_err());
    }
}
