//! 端点的共享模式格式：通过未公开的 `IPolicyConfig` 读取和设定（S2）。
//!
//! - 不需要管理员权限；设定后立即生效，拔出再插回后保留。
//! - 端点上已有的流会被作废（`AUDCLNT_E_DEVICE_INVALIDATED`），所以应在端点还没被应用使用时设定，
//!   或者事先提示用户。
//! - 混音格式取同采样率、同声道数的 32 位浮点，与声音控制面板的做法一致。
//!
//! 线程模型：[`PolicyConfig`] 持有 COM 接口指针，只能在创建它的线程上使用（须持有 [`crate::com::ComApartment`]）。

use aemeath_protocol::message::{DeviceFormat, SampleFormat};
use aemeath_protocol::units::SampleRate;
use thiserror::Error;
use windows::Win32::Foundation::FALSE;
use windows::Win32::Media::Audio::{WAVEFORMATEX, WAVEFORMATEXTENSIBLE, WAVEFORMATEXTENSIBLE_0};
use windows::Win32::Media::KernelStreaming::{KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE};
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree};
use windows::core::{GUID, HRESULT, HSTRING, IUnknown, IUnknown_Vtbl, PCWSTR, interface};

/// CLSID_PolicyConfigClient
const POLICY_CONFIG_CLIENT: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);
const WAVE_FORMAT_PCM: u16 = 1;
const BITS_PER_BYTE: u16 = 8;
/// WAVEFORMATEXTENSIBLE 在 WAVEFORMATEX 之后的附加字节数
const EXTENSIBLE_EXTRA_SIZE: u16 = 22;
/// 立体声的声道掩码（SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT）
const SPEAKER_STEREO: u32 = 0x3;
const STEREO_CHANNELS: u16 = 2;

/// 未公开的 IPolicyConfig（Windows 7 起的布局，Windows 10/11 沿用）。
/// 方法顺序就是虚表顺序，不能调整（名字无关，按 Rust 惯例写成 snake_case）；只声明到用到的 SetDeviceFormat 为止。
/// 返回的 WAVEFORMATEX 由 CoTaskMemAlloc 分配，调用方负责释放。
#[interface("f8679f50-850a-41cf-9c72-430f290290c8")]
unsafe trait IPolicyConfig: IUnknown {
    fn get_mix_format(&self, device_id: PCWSTR, format: *mut *mut WAVEFORMATEX) -> HRESULT;
    fn get_device_format(
        &self,
        device_id: PCWSTR,
        use_default: windows::core::BOOL,
        format: *mut *mut WAVEFORMATEX,
    ) -> HRESULT;
    fn reset_device_format(&self, device_id: PCWSTR) -> HRESULT;
    fn set_device_format(
        &self,
        device_id: PCWSTR,
        endpoint_format: *const WAVEFORMATEX,
        mix_format: *const WAVEFORMATEX,
    ) -> HRESULT;
}

#[derive(Debug, Error)]
pub enum FormatError {
    #[error("IPolicyConfig call failed: {0}")]
    Com(#[from] windows::core::Error),
    /// Windows 报告的格式无法用协议的 [`DeviceFormat`] 表示
    #[error("the endpoint format cannot be represented")]
    Unsupported,
}

pub struct PolicyConfig {
    config: IPolicyConfig,
}

impl std::fmt::Debug for PolicyConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PolicyConfig").finish_non_exhaustive()
    }
}

impl PolicyConfig {
    pub fn new() -> windows::core::Result<Self> {
        // SAFETY: 标准的 COM 创建调用；调用线程须已初始化 COM（见模块说明）
        let config = unsafe { CoCreateInstance(&POLICY_CONFIG_CLIENT, None, CLSCTX_ALL)? };
        Ok(Self { config })
    }

    /// 端点当前的设备格式（Windows 与设备收发用的格式）。
    pub fn device_format(&self, endpoint_id: &str) -> Result<DeviceFormat, FormatError> {
        let id = HSTRING::from(endpoint_id);
        let mut format: *mut WAVEFORMATEX = std::ptr::null_mut();
        // SAFETY: id 在调用期间有效；format 是输出参数
        unsafe { self.config.get_device_format(PCWSTR(id.as_ptr()), FALSE, &mut format) }.ok()?;
        if format.is_null() {
            return Err(FormatError::Unsupported);
        }
        // SAFETY: 成功时 format 指向一个有效的 WAVEFORMATEX；cbSize 足够时它是 WAVEFORMATEXTENSIBLE
        let parsed = unsafe { read_format(format) };
        // SAFETY: format 由 CoTaskMemAlloc 分配，所有权在调用方，读取后不再使用
        unsafe { CoTaskMemFree(Some(format as *const _)) };
        parsed.ok_or(FormatError::Unsupported)
    }

    /// 设定端点的设备格式（只支持立体声，v1 的槽位都是立体声）。
    pub fn set_device_format(&self, endpoint_id: &str, format: DeviceFormat) -> Result<(), FormatError> {
        if format.channels != STEREO_CHANNELS {
            return Err(FormatError::Unsupported);
        }
        let endpoint = to_extensible(format);
        let mix = to_extensible(DeviceFormat {
            sample_format: SampleFormat::Float32,
            ..format
        });
        let id = HSTRING::from(endpoint_id);
        // SAFETY: 两个格式都是完整的 WAVEFORMATEXTENSIBLE，以其开头的 WAVEFORMATEX 指针传入，调用期间有效
        unsafe {
            self.config.set_device_format(
                PCWSTR(id.as_ptr()),
                (&raw const endpoint).cast(),
                (&raw const mix).cast(),
            )
        }
        .ok()?;
        Ok(())
    }
}

/// 读取 WAVEFORMATEX / WAVEFORMATEXTENSIBLE。
///
/// # Safety
/// `format` 必须指向有效的 WAVEFORMATEX；`cbSize` 不小于 22 且格式标签为 EXTENSIBLE 时，
/// 后面必须跟着完整的扩展部分。
unsafe fn read_format(format: *const WAVEFORMATEX) -> Option<DeviceFormat> {
    // SAFETY: 由调用方保证；用非对齐读取，不假设系统分配的对齐
    let header = unsafe { format.read_unaligned() };
    let tag = header.wFormatTag;
    let (is_float, valid_bits) = if u32::from(tag) == WAVE_FORMAT_EXTENSIBLE && header.cbSize >= EXTENSIBLE_EXTRA_SIZE {
        // SAFETY: 由调用方保证扩展部分完整
        let extensible = unsafe { format.cast::<WAVEFORMATEXTENSIBLE>().read_unaligned() };
        // SAFETY: 联合体的三个成员都是 u16，按 PCM 的含义读取有效位数
        let valid = unsafe { extensible.Samples.wValidBitsPerSample };
        // 结构体是 packed 的，先按值复制再比较，不取字段的引用
        let sub_format = extensible.SubFormat;
        (sub_format == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, valid)
    } else {
        (u32::from(tag) == WAVE_FORMAT_IEEE_FLOAT, header.wBitsPerSample)
    };
    if !is_float && tag != WAVE_FORMAT_PCM && u32::from(tag) != WAVE_FORMAT_EXTENSIBLE {
        return None;
    }
    let sample_format = sample_format_of(is_float, header.wBitsPerSample, valid_bits)?;
    Some(DeviceFormat {
        sample_rate: SampleRate::new(header.nSamplesPerSec).ok()?,
        sample_format,
        channels: header.nChannels,
    })
}

/// (是否浮点, 容器位数, 有效位数) → 样本格式
fn sample_format_of(is_float: bool, container_bits: u16, valid_bits: u16) -> Option<SampleFormat> {
    match (is_float, container_bits, valid_bits) {
        (true, 32, _) => Some(SampleFormat::Float32),
        (true, 64, _) => Some(SampleFormat::Float64),
        (false, 16, _) => Some(SampleFormat::Int16),
        (false, 24, _) => Some(SampleFormat::Int24),
        (false, 32, 24) => Some(SampleFormat::Int24In32),
        (false, 32, 32) => Some(SampleFormat::Int32),
        _ => None,
    }
}

/// 样本格式 → (是否浮点, 容器位数, 有效位数)
fn layout_of(format: SampleFormat) -> (bool, u16, u16) {
    match format {
        SampleFormat::Int16 => (false, 16, 16),
        SampleFormat::Int24 => (false, 24, 24),
        SampleFormat::Int24In32 => (false, 32, 24),
        SampleFormat::Int32 => (false, 32, 32),
        SampleFormat::Float32 => (true, 32, 32),
        SampleFormat::Float64 => (true, 64, 64),
    }
}

fn to_extensible(format: DeviceFormat) -> WAVEFORMATEXTENSIBLE {
    let (is_float, container_bits, valid_bits) = layout_of(format.sample_format);
    let block_align = format.channels * container_bits / BITS_PER_BYTE;
    let sample_rate = format.sample_rate.hz();
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            // 格式标签是 0xFFFE，windows crate 把它声明成 u32，放进 u16 不会截断
            wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
            nChannels: format.channels,
            nSamplesPerSec: sample_rate,
            nAvgBytesPerSec: sample_rate * u32::from(block_align),
            nBlockAlign: block_align,
            wBitsPerSample: container_bits,
            cbSize: EXTENSIBLE_EXTRA_SIZE,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 {
            wValidBitsPerSample: valid_bits,
        },
        dwChannelMask: SPEAKER_STEREO,
        SubFormat: if is_float {
            KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
        } else {
            KSDATAFORMAT_SUBTYPE_PCM
        },
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

    #[test]
    fn every_sample_format_round_trips_through_waveformatextensible() {
        for sample_format in ALL_FORMATS {
            let format = DeviceFormat {
                sample_rate: SampleRate::HZ_48000,
                sample_format,
                channels: STEREO_CHANNELS,
            };
            let native = to_extensible(format);
            // SAFETY: native 是完整的 WAVEFORMATEXTENSIBLE
            let parsed = unsafe { read_format((&raw const native).cast()) };
            assert_eq!(parsed, Some(format), "{sample_format:?}");
        }
    }

    #[test]
    fn plain_waveformatex_is_understood() {
        let header = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM,
            nChannels: STEREO_CHANNELS,
            nSamplesPerSec: 44_100,
            nAvgBytesPerSec: 44_100 * 4,
            nBlockAlign: 4,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        // SAFETY: header 是有效的 WAVEFORMATEX，cbSize 为 0
        let parsed = unsafe { read_format(&raw const header) }.map(|format| format.sample_format);
        assert_eq!(parsed, Some(SampleFormat::Int16));
    }

    #[test]
    fn block_align_and_byte_rate_follow_the_container() {
        let native = to_extensible(DeviceFormat {
            sample_rate: SampleRate::HZ_48000,
            sample_format: SampleFormat::Int24In32,
            channels: STEREO_CHANNELS,
        });
        let header = native.Format;
        assert_eq!({ header.nBlockAlign }, 8);
        assert_eq!({ header.nAvgBytesPerSec }, 48_000 * 8);
        assert_eq!({ header.wBitsPerSample }, 32);
    }
}
