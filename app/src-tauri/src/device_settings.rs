//! 外部设备节点的设备设置（ADR-0014）在界面上的表示：快照里发给界面，界面改完再原样传回来。
//!
//! 协议类型（`aemeath_protocol::topology::DeviceSettings`）用 snake_case 的带标签枚举，界面统一用 camelCase，
//! 所以在这里换算，并在换算时校验取值。

use aemeath_protocol::message::{DeviceCapabilities, DeviceFormat};
use aemeath_protocol::topology::{
    BufferChoice, DeviceAccess, DeviceSettings, FftPreset, FormatChoice, PolyPreset, Resampler, SincPreset,
    SlotFormatChoice, SlotNodeSettings,
};
use aemeath_protocol::units::{BufferMs, SampleRate};
use serde::{Deserialize, Serialize};

use crate::view::FormatView;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSettingsView {
    pub exclusive: bool,
    /// 独占时的格式；None 表示推荐格式
    pub format: Option<FormatView>,
    pub buffer: BufferView,
    /// 按节点覆盖的重采样算法；None 表示跟随全局默认
    pub resampler: Option<ResamplerView>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "choice", rename_all = "camelCase")]
pub enum BufferView {
    DeviceDefault,
    DeviceMinimum,
    Milliseconds { ms: f32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "algorithm", rename_all = "camelCase")]
pub enum ResamplerView {
    PolyAsync { preset: PolyPresetView },
    SincAsync { preset: SincPresetView },
    FftSync { preset: FftPresetView },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PolyPresetView {
    Fastest,
    Smooth,
    Fine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SincPresetView {
    VeryFast,
    Fast,
    Balanced,
    Accurate,
    Ultra,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FftPresetView {
    LowLatency,
    Balanced,
}

impl From<Resampler> for ResamplerView {
    fn from(resampler: Resampler) -> Self {
        match resampler {
            Resampler::PolyAsync { preset } => Self::PolyAsync {
                preset: match preset {
                    PolyPreset::Fastest => PolyPresetView::Fastest,
                    PolyPreset::Smooth => PolyPresetView::Smooth,
                    PolyPreset::Fine => PolyPresetView::Fine,
                },
            },
            Resampler::SincAsync { preset } => Self::SincAsync {
                preset: match preset {
                    SincPreset::VeryFast => SincPresetView::VeryFast,
                    SincPreset::Fast => SincPresetView::Fast,
                    SincPreset::Balanced => SincPresetView::Balanced,
                    SincPreset::Accurate => SincPresetView::Accurate,
                    SincPreset::Ultra => SincPresetView::Ultra,
                },
            },
            Resampler::FftSync { preset } => Self::FftSync {
                preset: match preset {
                    FftPreset::LowLatency => FftPresetView::LowLatency,
                    FftPreset::Balanced => FftPresetView::Balanced,
                },
            },
        }
    }
}

impl From<ResamplerView> for Resampler {
    fn from(resampler: ResamplerView) -> Self {
        match resampler {
            ResamplerView::PolyAsync { preset } => Self::PolyAsync {
                preset: match preset {
                    PolyPresetView::Fastest => PolyPreset::Fastest,
                    PolyPresetView::Smooth => PolyPreset::Smooth,
                    PolyPresetView::Fine => PolyPreset::Fine,
                },
            },
            ResamplerView::SincAsync { preset } => Self::SincAsync {
                preset: match preset {
                    SincPresetView::VeryFast => SincPreset::VeryFast,
                    SincPresetView::Fast => SincPreset::Fast,
                    SincPresetView::Balanced => SincPreset::Balanced,
                    SincPresetView::Accurate => SincPreset::Accurate,
                    SincPresetView::Ultra => SincPreset::Ultra,
                },
            },
            ResamplerView::FftSync { preset } => Self::FftSync {
                preset: match preset {
                    FftPresetView::LowLatency => FftPreset::LowLatency,
                    FftPresetView::Balanced => FftPreset::Balanced,
                },
            },
        }
    }
}

impl From<DeviceSettings> for DeviceSettingsView {
    fn from(settings: DeviceSettings) -> Self {
        let (exclusive, format, buffer) = match settings.access {
            DeviceAccess::Shared => (false, None, BufferView::DeviceDefault),
            DeviceAccess::Exclusive { format, buffer } => (
                true,
                match format {
                    FormatChoice::Recommended => None,
                    FormatChoice::Fixed { format } => Some(FormatView::from(format)),
                },
                match buffer {
                    BufferChoice::DeviceDefault => BufferView::DeviceDefault,
                    BufferChoice::DeviceMinimum => BufferView::DeviceMinimum,
                    BufferChoice::Milliseconds { ms } => BufferView::Milliseconds { ms: ms.ms() },
                },
            ),
        };
        Self {
            exclusive,
            format,
            buffer,
            resampler: settings.resampler.map(ResamplerView::from),
        }
    }
}

/// 界面传回的设置不合法（采样率或缓冲超出范围）。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("invalid device settings: {0}")]
pub struct InvalidSettings(String);

impl TryFrom<DeviceSettingsView> for DeviceSettings {
    type Error = InvalidSettings;

    fn try_from(view: DeviceSettingsView) -> Result<Self, Self::Error> {
        let access = if view.exclusive {
            let format = match view.format {
                None => FormatChoice::Recommended,
                Some(format) => FormatChoice::Fixed {
                    format: DeviceFormat::try_from(format).map_err(|error| InvalidSettings(error.to_string()))?,
                },
            };
            let buffer = match view.buffer {
                BufferView::DeviceDefault => BufferChoice::DeviceDefault,
                BufferView::DeviceMinimum => BufferChoice::DeviceMinimum,
                BufferView::Milliseconds { ms } => BufferChoice::Milliseconds {
                    ms: BufferMs::new(ms).map_err(|error| InvalidSettings(error.to_string()))?,
                },
            };
            DeviceAccess::Exclusive { format, buffer }
        } else {
            DeviceAccess::Shared
        };
        Ok(Self {
            access,
            resampler: view.resampler.map(Resampler::from),
        })
    }
}

/// 占用槽位的节点的设置在界面上的表示。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotSettingsView {
    /// None 表示跟随引擎
    pub format: Option<FormatView>,
    /// None 表示跟随全局默认
    pub resampler: Option<ResamplerView>,
}

/// 槽位都是立体声；界面的格式带声道数，这里只用于显示
const SLOT_CHANNELS: u16 = 2;

impl From<SlotNodeSettings> for SlotSettingsView {
    fn from(settings: SlotNodeSettings) -> Self {
        Self {
            format: match settings.format {
                SlotFormatChoice::FollowEngine => None,
                SlotFormatChoice::Fixed {
                    sample_rate,
                    sample_format,
                } => Some(FormatView::from(DeviceFormat {
                    sample_rate,
                    sample_format,
                    channels: SLOT_CHANNELS,
                })),
            },
            resampler: settings.resampler.map(ResamplerView::from),
        }
    }
}

impl TryFrom<SlotSettingsView> for SlotNodeSettings {
    type Error = InvalidSettings;

    fn try_from(view: SlotSettingsView) -> Result<Self, Self::Error> {
        let format = match view.format {
            None => SlotFormatChoice::FollowEngine,
            Some(format) => {
                let format = DeviceFormat::try_from(format).map_err(|error| InvalidSettings(error.to_string()))?;
                SlotFormatChoice::Fixed {
                    sample_rate: format.sample_rate,
                    sample_format: format.sample_format,
                }
            }
        };
        if !format.is_valid() {
            return Err(InvalidSettings("the virtual device cannot use this format".to_owned()));
        }
        Ok(Self {
            format,
            resampler: view.resampler.map(Resampler::from),
        })
    }
}

/// 设备能力在界面上的表示（添加节点与节点设置里的格式、缓冲选项）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilitiesView {
    pub exclusive_formats: Vec<FormatView>,
    /// 推荐的独占格式（[`DeviceCapabilities::recommended`]）
    pub recommended: Option<FormatView>,
    pub shared_format: Option<FormatView>,
    pub default_period_ms: f32,
    pub minimum_period_ms: f32,
}

const MICROSECONDS_PER_MILLISECOND: f32 = 1000.0;

impl CapabilitiesView {
    pub fn new(capabilities: &DeviceCapabilities, engine_rate: SampleRate) -> Self {
        Self {
            exclusive_formats: capabilities
                .exclusive_formats
                .iter()
                .copied()
                .map(FormatView::from)
                .collect(),
            recommended: capabilities.recommended(engine_rate).map(FormatView::from),
            shared_format: capabilities.shared_format.map(FormatView::from),
            default_period_ms: capabilities.default_period_us as f32 / MICROSECONDS_PER_MILLISECOND,
            minimum_period_ms: capabilities.minimum_period_us as f32 / MICROSECONDS_PER_MILLISECOND,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aemeath_protocol::message::SampleFormat;

    #[test]
    fn settings_round_trip_through_the_view() {
        let settings = DeviceSettings {
            access: DeviceAccess::Exclusive {
                format: FormatChoice::Fixed {
                    format: DeviceFormat {
                        sample_rate: SampleRate::HZ_48000,
                        sample_format: SampleFormat::Int24In32,
                        channels: 2,
                    },
                },
                buffer: BufferChoice::Milliseconds {
                    ms: BufferMs::new(3.0).unwrap(),
                },
            },
            resampler: Some(Resampler::FftSync {
                preset: FftPreset::LowLatency,
            }),
        };
        let view = DeviceSettingsView::from(settings);
        assert_eq!(DeviceSettings::try_from(view), Ok(settings));
        assert_eq!(
            serde_json::to_string(&view).unwrap(),
            r#"{"exclusive":true,"format":{"sampleRate":48000,"sampleFormat":"int24In32","channels":2},"buffer":{"choice":"milliseconds","ms":3.0},"resampler":{"algorithm":"fftSync","preset":"lowLatency"}}"#
        );
    }

    #[test]
    fn rejects_out_of_range_values() {
        let view = DeviceSettingsView {
            exclusive: true,
            format: None,
            buffer: BufferView::Milliseconds { ms: 0.0 },
            resampler: None,
        };
        assert!(DeviceSettings::try_from(view).is_err());
    }
}
