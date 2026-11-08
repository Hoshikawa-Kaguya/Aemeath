//! WASAPI 的 `WaveFormat` 与协议的 [`DeviceFormat`] 之间的换算。

use aemeath_protocol::message::{DeviceFormat, SampleFormat};
use aemeath_protocol::units::SampleRate;
use wasapi::{SampleType, WaveFormat};

/// 样本格式的容器位数、有效位数和类型。
struct Layout {
    store_bits: usize,
    valid_bits: usize,
    sample_type: SampleType,
}

fn layout(format: SampleFormat) -> Layout {
    let (store_bits, valid_bits, sample_type) = match format {
        SampleFormat::Int16 => (16, 16, SampleType::Int),
        SampleFormat::Int24 => (24, 24, SampleType::Int),
        SampleFormat::Int24In32 => (32, 24, SampleType::Int),
        SampleFormat::Int32 => (32, 32, SampleType::Int),
        SampleFormat::Float32 => (32, 32, SampleType::Float),
        SampleFormat::Float64 => (64, 64, SampleType::Float),
    };
    Layout {
        store_bits,
        valid_bits,
        sample_type,
    }
}

/// 协议格式 → WASAPI 格式（WAVEFORMATEXTENSIBLE，默认声道掩码）。
pub fn to_wave_format(format: DeviceFormat) -> WaveFormat {
    let layout = layout(format.sample_format);
    WaveFormat::new(
        layout.store_bits,
        layout.valid_bits,
        layout.sample_type,
        format.sample_rate.hz() as usize,
        usize::from(format.channels),
        None,
    )
}

/// WASAPI 格式 → 协议格式；协议无法表示的格式（例如 20 位、8 位）返回 None。
pub fn to_device_format(format: &WaveFormat) -> Option<DeviceFormat> {
    let store_bits = format.get_bitspersample();
    let valid_bits = format.get_validbitspersample();
    let sample_format = match (format.get_subformat().ok()?, store_bits, valid_bits) {
        (SampleType::Float, 32, _) => SampleFormat::Float32,
        (SampleType::Float, 64, _) => SampleFormat::Float64,
        (SampleType::Int, 16, 16) => SampleFormat::Int16,
        (SampleType::Int, 24, 24) => SampleFormat::Int24,
        (SampleType::Int, 32, 24) => SampleFormat::Int24In32,
        (SampleType::Int, 32, 32) => SampleFormat::Int32,
        _ => return None,
    };
    Some(DeviceFormat {
        sample_rate: SampleRate::new(format.get_samplespersec()).ok()?,
        sample_format,
        channels: format.get_nchannels(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_sample_format_round_trips() {
        for sample_format in [
            SampleFormat::Int16,
            SampleFormat::Int24,
            SampleFormat::Int24In32,
            SampleFormat::Int32,
            SampleFormat::Float32,
            SampleFormat::Float64,
        ] {
            let format = DeviceFormat {
                sample_rate: SampleRate::HZ_48000,
                sample_format,
                channels: 2,
            };
            assert_eq!(to_device_format(&to_wave_format(format)), Some(format));
        }
    }
}
