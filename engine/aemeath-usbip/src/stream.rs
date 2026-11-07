//! 流格式与等时包的节拍。

use aemeath_protocol::message::SampleFormat;
use aemeath_protocol::units::SampleRate;

/// 立体声（v1 的槽位都是立体声；N 声道节点见选型报告第 10 节）
pub const CHANNELS: u8 = 2;

/// 高速 USB 每秒的微帧数（125 µs 一个）
pub const MICROFRAMES_PER_SECOND: u64 = 8_000;

/// 槽位声明的全部采样率（ADR-0010：声明全部格式，Windows 选哪个就按哪个收发）。
pub const SUPPORTED_RATES: [u32; 10] = [
    44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000, 705_600, 768_000,
];

/// 槽位首次被主机读取时报告的采样率
pub const INITIAL_RATE: SampleRate = SampleRate::HZ_48000;

/// 一种样本格式在 USB 上的编码：每个样本占的字节数（subslot）和有效位数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WireFormat {
    pub sample_format: SampleFormat,
    pub subslot_bytes: u8,
    pub bit_resolution: u8,
    pub is_float: bool,
}

/// 流接口的备用设置（alternate setting）。备用设置 0 表示停止，1–5 依次对应这些格式。
/// 顺序与探针一致，不能调整（主机按编号选择格式）。
pub const WIRE_FORMATS: [WireFormat; 5] = [
    WireFormat {
        sample_format: SampleFormat::Int16,
        subslot_bytes: 2,
        bit_resolution: 16,
        is_float: false,
    },
    WireFormat {
        sample_format: SampleFormat::Int24,
        subslot_bytes: 3,
        bit_resolution: 24,
        is_float: false,
    },
    WireFormat {
        sample_format: SampleFormat::Int32,
        subslot_bytes: 4,
        bit_resolution: 32,
        is_float: false,
    },
    WireFormat {
        sample_format: SampleFormat::Float32,
        subslot_bytes: 4,
        bit_resolution: 32,
        is_float: true,
    },
    WireFormat {
        sample_format: SampleFormat::Int24In32,
        subslot_bytes: 4,
        bit_resolution: 24,
        is_float: false,
    },
];

/// 主机选定的流格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamFormat {
    pub sample_rate: SampleRate,
    pub wire: WireFormat,
}

impl StreamFormat {
    /// 每个采样帧（全部声道）的字节数
    pub fn bytes_per_frame(&self) -> usize {
        usize::from(self.wire.subslot_bytes) * usize::from(CHANNELS)
    }
}

/// 录音方向（设备发给主机）每个等时包放多少帧。
///
/// 每个微帧应送出 `采样率 / 8000` 帧；44.1k 这类不能整除的采样率靠余数累计，
/// 长期平均严格等于采样率，不会漂移。
#[derive(Debug, Clone, Default)]
pub struct CapturePacer {
    remainder: u64,
}

impl CapturePacer {
    /// 下一个包应放的帧数；`interval_microframes` 是这个包覆盖的微帧数（URB 的 interval）。
    pub fn next_packet_frames(&mut self, sample_rate: SampleRate, interval_microframes: u32) -> usize {
        self.remainder += u64::from(sample_rate.hz()) * u64::from(interval_microframes.max(1));
        let frames = self.remainder / MICROFRAMES_PER_SECOND;
        self.remainder %= MICROFRAMES_PER_SECOND;
        usize::try_from(frames).unwrap_or(usize::MAX)
    }

    /// 流重新开始时清零。
    pub fn reset(&mut self) {
        self.remainder = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pacer_delivers_exactly_the_sample_rate_over_one_second() {
        for rate in SUPPORTED_RATES {
            let mut pacer = CapturePacer::default();
            let sample_rate = SampleRate::new(rate).unwrap();
            let total: usize = (0..MICROFRAMES_PER_SECOND)
                .map(|_| pacer.next_packet_frames(sample_rate, 1))
                .sum();
            assert_eq!(total, rate as usize, "rate {rate}");
        }
    }

    #[test]
    fn pacer_spreads_44k1_as_5_or_6_frames_per_microframe() {
        let mut pacer = CapturePacer::default();
        let rate = SampleRate::new(44_100).unwrap();
        let frames: Vec<usize> = (0..8).map(|_| pacer.next_packet_frames(rate, 1)).collect();
        assert!(frames.iter().all(|count| *count == 5 || *count == 6), "{frames:?}");
    }

    #[test]
    fn bytes_per_frame_counts_both_channels() {
        let format = StreamFormat {
            sample_rate: SampleRate::new(48_000).unwrap(),
            wire: WIRE_FORMATS[1],
        };
        assert_eq!(format.bytes_per_frame(), 6);
    }
}
