//! 槽位的 USB Audio Class 2.0 描述符（高速设备）。
//!
//! 这份字节布局已在 Windows 上实际注册过开发槽位，内置的 USB 声卡驱动能正确识别。**描述符的任何改动都要同时修改
//! `identity::DEVICE_RELEASE`**，因为 Windows 会按"VID/PID + 版本号"缓存部分描述符。
//!
//! 每个槽位只有一个方向（Windows 的 usbaudio2 驱动每台设备只支持一个时钟源，同一台设备上的播放和录音无法各用各的时钟）：
//! - 输入槽位：应用往里播放。USB 流输入终端 → 扬声器输出终端，等时 OUT 端点，自适应同步。
//! - 输出槽位：OBS 等从里面录。线路输入终端→ USB 流输出终端，等时 IN 端点，异步同步。

use aemeath_protocol::ids::SlotDirection;

use crate::identity::SlotIdentity;
use crate::stream::{CHANNELS, SUPPORTED_RATES, WIRE_FORMATS};
use crate::wire::ClassTriple;

// 标准描述符类型
const DESC_DEVICE: u8 = 0x01;
const DESC_CONFIGURATION: u8 = 0x02;
const DESC_INTERFACE: u8 = 0x04;
const DESC_ENDPOINT: u8 = 0x05;
const DESC_QUALIFIER: u8 = 0x06;
const DESC_INTERFACE_ASSOCIATION: u8 = 0x0B;
// 音频类专用描述符类型与子类型
const CS_INTERFACE: u8 = 0x24;
const CS_ENDPOINT: u8 = 0x25;
const AC_HEADER: u8 = 0x01;
const AC_INPUT_TERMINAL: u8 = 0x02;
const AC_OUTPUT_TERMINAL: u8 = 0x03;
const AC_CLOCK_SOURCE: u8 = 0x0A;
const AS_GENERAL: u8 = 0x01;
const AS_FORMAT_TYPE: u8 = 0x02;
const EP_GENERAL: u8 = 0x01;

const USB_2_0: u16 = 0x0200;
const AUDIO_CLASS: u8 = 0x01;
const AUDIO_CONTROL: u8 = 0x01;
const AUDIO_STREAMING: u8 = 0x02;
const AUDIO_2_0_PROTOCOL: u8 = 0x20;
/// 设备类"杂项 / 公共类 / 接口关联"：复合设备用接口关联描述符组织功能
const MISC_CLASS: ClassTriple = ClassTriple {
    class: 0xEF,
    subclass: 0x02,
    protocol: 0x01,
};
const EP0_MAX_PACKET: u8 = 64;

/// 接口号：0 是音频控制，1 是音频流
pub const CONTROL_INTERFACE: u8 = 0;
pub const STREAMING_INTERFACE: u8 = 1;
const INTERFACE_COUNT: u8 = 2;

// 音频功能内的实体 ID（输入、输出槽位各用其中两个终端）。改动它们就是改动描述符，见模块说明
pub const CLOCK_ID: u8 = 0x10;
const RENDER_INPUT_TERMINAL: u8 = 0x01;
const RENDER_OUTPUT_TERMINAL: u8 = 0x02;
const CAPTURE_INPUT_TERMINAL: u8 = 0x03;
const CAPTURE_OUTPUT_TERMINAL: u8 = 0x04;

// 终端类型
const TERMINAL_USB_STREAMING: u16 = 0x0101;
const TERMINAL_SPEAKER: u16 = 0x0301;
/// 线路连接器：虚拟输出必须声明为线路，否则 Windows 会套用麦克风处理
const TERMINAL_LINE_CONNECTOR: u16 = 0x0603;

/// 内部可编程时钟；采样率可读写、时钟有效性只读
const CLOCK_INTERNAL_PROGRAMMABLE: u8 = 0x03;
const CLOCK_CONTROLS: u8 = 0x07;
/// 音频功能类别：I/O 盒
const FUNCTION_CATEGORY_IO_BOX: u8 = 0x08;
/// 声道配置：前左、前右
const CHANNEL_CONFIG_STEREO: u32 = 0x0000_0003;
const FORMAT_TYPE_I: u8 = 0x01;
const FORMAT_PCM: u32 = 0x0000_0001;
const FORMAT_IEEE_FLOAT: u32 = 0x0000_0004;

/// 播放（OUT）和录音（IN，最高位为 1）等时端点的地址
pub(crate) const RENDER_ENDPOINT: u8 = 0x01;
pub(crate) const CAPTURE_ENDPOINT: u8 = 0x82;
/// 端点地址的低 4 位是端点号（URB 里只带端点号）
pub(crate) const ENDPOINT_NUMBER_MASK: u8 = 0x0F;
/// 等时传输、自适应同步（播放端由主机控制速率）
const ISO_ADAPTIVE: u8 = 0x09;
/// 等时传输、异步同步（录音端由设备控制速率）
const ISO_ASYNC: u8 = 0x05;
/// 每个微帧一次
const ENDPOINT_INTERVAL: u8 = 1;
/// 高速等时端点单个包的上限
const MAX_ISO_PACKET: u64 = 1024;

const CONFIGURATION_VALUE: u8 = 1;
/// 总线供电
const CONFIG_ATTRIBUTES: u8 = 0x80;
/// 100 mA（单位是 2 mA）
const CONFIG_MAX_POWER: u8 = 50;

/// 字符串描述符的编号（`strings` 中的位置加一）
const STRING_MANUFACTURER: u8 = 1;
const STRING_PRODUCT: u8 = 2;
const STRING_SERIAL: u8 = 3;
const STRING_FUNCTION: u8 = 4;
const STRING_CLOCK: u8 = 5;
const STRING_PLAYBACK: u8 = 6;
const STRING_RECORDING: u8 = 7;
const STRING_USB_STREAMING: u8 = 8;

/// 一个槽位的全部描述符。
#[derive(Debug, Clone)]
pub struct Descriptors {
    pub device: Vec<u8>,
    pub configuration: Vec<u8>,
    pub qualifier: Vec<u8>,
    /// 字符串描述符的内容，编号从 1 开始
    pub strings: Vec<String>,
    pub device_class: ClassTriple,
    pub interface_classes: Vec<ClassTriple>,
}

impl Descriptors {
    /// 按槽位身份生成全套描述符：方向决定是扬声器还是线路，身份决定 VID/PID、版本号和字符串。
    pub fn build(identity: &SlotIdentity) -> Self {
        let mut device = vec![18, DESC_DEVICE];
        put_le16(&mut device, USB_2_0);
        device.extend_from_slice(&[
            MISC_CLASS.class,
            MISC_CLASS.subclass,
            MISC_CLASS.protocol,
            EP0_MAX_PACKET,
        ]);
        put_le16(&mut device, identity.vendor_id);
        put_le16(&mut device, identity.product_id);
        put_le16(&mut device, identity.device_release);
        device.extend_from_slice(&[STRING_MANUFACTURER, STRING_PRODUCT, STRING_SERIAL, 1]);

        let mut qualifier = vec![10, DESC_QUALIFIER];
        put_le16(&mut qualifier, USB_2_0);
        qualifier.extend_from_slice(&[
            MISC_CLASS.class,
            MISC_CLASS.subclass,
            MISC_CLASS.protocol,
            EP0_MAX_PACKET,
            1,
            0,
        ]);

        let name = &identity.product_name;
        let strings = vec![
            identity.manufacturer.clone(),
            name.clone(),
            identity.serial_number.clone(),
            name.clone(),
            "Aemeath Clock".to_owned(),
            format!("{name} Playback"),
            format!("{name} Recording"),
            "USB Streaming".to_owned(),
        ];

        Self {
            device,
            configuration: configuration(identity.slot.direction),
            qualifier,
            strings,
            device_class: MISC_CLASS,
            interface_classes: vec![
                ClassTriple {
                    class: AUDIO_CLASS,
                    subclass: AUDIO_CONTROL,
                    protocol: AUDIO_2_0_PROTOCOL,
                },
                ClassTriple {
                    class: AUDIO_CLASS,
                    subclass: AUDIO_STREAMING,
                    protocol: AUDIO_2_0_PROTOCOL,
                },
            ],
        }
    }

    /// 字符串描述符（UTF-16LE）；编号 0 是语言列表（只有美式英语）。
    pub fn string(&self, index: u8) -> Option<Vec<u8>> {
        const LANGUAGE_EN_US: u16 = 0x0409;
        const DESC_STRING: u8 = 0x03;
        let mut out = vec![0, DESC_STRING];
        if index == 0 {
            put_le16(&mut out, LANGUAGE_EN_US);
        } else {
            let text = self.strings.get(usize::from(index) - 1)?;
            for unit in text.encode_utf16() {
                put_le16(&mut out, unit);
            }
        }
        out[0] = u8::try_from(out.len()).ok()?;
        Some(out)
    }
}

/// 配置描述符及其下的全部接口、音频类和端点描述符，按方向生成。
fn configuration(direction: SlotDirection) -> Vec<u8> {
    let mut audio_control = Vec::new();
    audio_control.extend_from_slice(&[
        8,
        CS_INTERFACE,
        AC_CLOCK_SOURCE,
        CLOCK_ID,
        CLOCK_INTERNAL_PROGRAMMABLE,
        CLOCK_CONTROLS,
        0,
        STRING_CLOCK,
    ]);
    let (input_terminal, input_type, input_string, output_terminal, output_type, output_string) = match direction {
        SlotDirection::In => (
            RENDER_INPUT_TERMINAL,
            TERMINAL_USB_STREAMING,
            STRING_USB_STREAMING,
            RENDER_OUTPUT_TERMINAL,
            TERMINAL_SPEAKER,
            STRING_PLAYBACK,
        ),
        SlotDirection::Out => (
            CAPTURE_INPUT_TERMINAL,
            TERMINAL_LINE_CONNECTOR,
            STRING_RECORDING,
            CAPTURE_OUTPUT_TERMINAL,
            TERMINAL_USB_STREAMING,
            STRING_USB_STREAMING,
        ),
    };
    // 输入终端：ID、类型、关联终端、时钟、声道数、声道配置、声道名、控制、字符串
    audio_control.extend_from_slice(&[17, CS_INTERFACE, AC_INPUT_TERMINAL, input_terminal]);
    put_le16(&mut audio_control, input_type);
    audio_control.extend_from_slice(&[0, CLOCK_ID, CHANNELS]);
    put_le32(&mut audio_control, CHANNEL_CONFIG_STEREO);
    audio_control.push(0);
    put_le16(&mut audio_control, 0);
    audio_control.push(input_string);
    // 输出终端：ID、类型、关联终端、信号来源、时钟、控制、字符串
    audio_control.extend_from_slice(&[12, CS_INTERFACE, AC_OUTPUT_TERMINAL, output_terminal]);
    put_le16(&mut audio_control, output_type);
    audio_control.extend_from_slice(&[0, input_terminal, CLOCK_ID]);
    put_le16(&mut audio_control, 0);
    audio_control.push(output_string);

    let header_len: u16 = 9;
    let mut header = vec![9, CS_INTERFACE, AC_HEADER];
    put_le16(&mut header, USB_2_0);
    header.push(FUNCTION_CATEGORY_IO_BOX);
    put_le16(&mut header, header_len + len_u16(&audio_control));
    header.push(0);

    let mut body = vec![
        8,
        DESC_INTERFACE_ASSOCIATION,
        CONTROL_INTERFACE,
        INTERFACE_COUNT,
        AUDIO_CLASS,
        0,
        AUDIO_2_0_PROTOCOL,
        STRING_FUNCTION,
    ];
    body.extend_from_slice(&[
        9,
        DESC_INTERFACE,
        CONTROL_INTERFACE,
        0,
        0,
        AUDIO_CLASS,
        AUDIO_CONTROL,
        AUDIO_2_0_PROTOCOL,
        0,
    ]);
    body.extend_from_slice(&header);
    body.extend_from_slice(&audio_control);

    let (link_terminal, endpoint, attributes) = match direction {
        SlotDirection::In => (RENDER_INPUT_TERMINAL, RENDER_ENDPOINT, ISO_ADAPTIVE),
        SlotDirection::Out => (CAPTURE_OUTPUT_TERMINAL, CAPTURE_ENDPOINT, ISO_ASYNC),
    };
    // 备用设置 0：没有端点，表示停止
    body.extend_from_slice(&[
        9,
        DESC_INTERFACE,
        STREAMING_INTERFACE,
        0,
        0,
        AUDIO_CLASS,
        AUDIO_STREAMING,
        AUDIO_2_0_PROTOCOL,
        0,
    ]);
    for (index, format) in WIRE_FORMATS.iter().enumerate() {
        let alternate = u8::try_from(index + 1).unwrap_or(u8::MAX);
        body.extend_from_slice(&[
            9,
            DESC_INTERFACE,
            STREAMING_INTERFACE,
            alternate,
            1,
            AUDIO_CLASS,
            AUDIO_STREAMING,
            AUDIO_2_0_PROTOCOL,
            0,
        ]);
        body.extend_from_slice(&[16, CS_INTERFACE, AS_GENERAL, link_terminal, 0, FORMAT_TYPE_I]);
        put_le32(&mut body, if format.is_float { FORMAT_IEEE_FLOAT } else { FORMAT_PCM });
        body.push(CHANNELS);
        put_le32(&mut body, CHANNEL_CONFIG_STEREO);
        body.push(0);
        body.extend_from_slice(&[
            6,
            CS_INTERFACE,
            AS_FORMAT_TYPE,
            FORMAT_TYPE_I,
            format.subslot_bytes,
            format.bit_resolution,
        ]);
        body.extend_from_slice(&[7, DESC_ENDPOINT, endpoint, attributes]);
        put_le16(&mut body, max_packet_size(format.subslot_bytes));
        body.push(ENDPOINT_INTERVAL);
        body.extend_from_slice(&[8, CS_ENDPOINT, EP_GENERAL, 0, 0, 0, 0, 0]);
    }

    let config_header_len: u16 = 9;
    let mut configuration = vec![9, DESC_CONFIGURATION];
    put_le16(&mut configuration, config_header_len + len_u16(&body));
    configuration.extend_from_slice(&[
        INTERFACE_COUNT,
        CONFIGURATION_VALUE,
        0,
        CONFIG_ATTRIBUTES,
        CONFIG_MAX_POWER,
    ]);
    configuration.extend_from_slice(&body);
    configuration
}

/// 等时包的最大字节数：最高采样率下每个微帧的帧数再加一帧余量（44.1k 系列不能整除）。
fn max_packet_size(subslot_bytes: u8) -> u16 {
    let max_rate = SUPPORTED_RATES.iter().copied().max().unwrap_or(48_000);
    let frames = u64::from(max_rate).div_ceil(crate::stream::MICROFRAMES_PER_SECOND) + 1;
    let bytes = frames * u64::from(subslot_bytes) * u64::from(CHANNELS);
    u16::try_from(bytes.min(MAX_ISO_PACKET)).unwrap_or(u16::MAX)
}

fn len_u16(bytes: &[u8]) -> u16 {
    u16::try_from(bytes.len()).unwrap_or(u16::MAX)
}

fn put_le16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_le32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::IdentityProfile;
    use aemeath_protocol::ids::{SlotId, SlotNumber};

    fn descriptors(direction: SlotDirection) -> Descriptors {
        let slot = SlotId {
            direction,
            number: SlotNumber::new(1).unwrap(),
        };
        Descriptors::build(&IdentityProfile::Development.identity(slot).unwrap())
    }

    /// 按 bLength 逐个走完配置描述符，必须正好落在 wTotalLength 上
    fn walk(configuration: &[u8]) -> Vec<(u8, u8)> {
        let total = usize::from(u16::from_le_bytes([configuration[2], configuration[3]]));
        assert_eq!(total, configuration.len());
        let mut pos = 0;
        let mut found = Vec::new();
        while pos < configuration.len() {
            let len = usize::from(configuration[pos]);
            assert!(len >= 2, "zero-length descriptor at {pos}");
            found.push((configuration[pos + 1], configuration.get(pos + 2).copied().unwrap_or(0)));
            pos += len;
        }
        assert_eq!(pos, total);
        found
    }

    #[test]
    fn configuration_descriptors_are_well_formed() {
        for direction in [SlotDirection::In, SlotDirection::Out] {
            let found = walk(&descriptors(direction).configuration);
            let interfaces = found.iter().filter(|(kind, _)| *kind == DESC_INTERFACE).count();
            // 音频控制 1 个 + 流接口的备用设置 0–5
            assert_eq!(interfaces, 1 + 1 + WIRE_FORMATS.len());
            let endpoints = found.iter().filter(|(kind, _)| *kind == DESC_ENDPOINT).count();
            assert_eq!(endpoints, WIRE_FORMATS.len());
        }
    }

    #[test]
    fn output_slots_declare_a_line_connector() {
        let configuration = descriptors(SlotDirection::Out).configuration;
        let line = TERMINAL_LINE_CONNECTOR.to_le_bytes();
        let has_line_input = configuration.windows(6).any(|window| {
            window[1..4] == [CS_INTERFACE, AC_INPUT_TERMINAL, CAPTURE_INPUT_TERMINAL] && window[4..6] == line
        });
        assert!(has_line_input);
    }

    #[test]
    fn input_slots_declare_a_speaker() {
        let configuration = descriptors(SlotDirection::In).configuration;
        let speaker = TERMINAL_SPEAKER.to_le_bytes();
        let has_speaker = configuration.windows(6).any(|window| {
            window[1..4] == [CS_INTERFACE, AC_OUTPUT_TERMINAL, RENDER_OUTPUT_TERMINAL] && window[4..6] == speaker
        });
        assert!(has_speaker);
    }

    #[test]
    fn largest_packet_fits_768k_float_stereo() {
        // 768k / 8000 = 96 帧，加 1 帧余量，每帧 8 字节
        assert_eq!(max_packet_size(4), 97 * 8);
    }

    #[test]
    fn string_descriptors_are_utf16() {
        let descriptors = descriptors(SlotDirection::Out);
        assert_eq!(descriptors.string(0), Some(vec![4, 0x03, 0x09, 0x04]));
        let product = descriptors.string(STRING_PRODUCT).unwrap();
        assert_eq!(usize::from(product[0]), product.len());
        assert_eq!(product.len(), 2 + "Aemeath Dev Out 1".len() * 2);
        assert_eq!(descriptors.string(99), None);
    }
}
