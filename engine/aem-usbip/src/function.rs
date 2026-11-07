//! 一个槽位的 UAC2 音频功能：处理控制端点（ep0）的请求，并跟踪主机选定的流格式。
//!
//! 只处理状态，不做 IO，也不涉及时间。流的开始、停止和格式变化以 [`StreamChange`] 告诉调用方，
//! 由引擎的调度线程据此收发等时数据。

use aem_protocol::ids::SlotDirection;
use aem_protocol::units::SampleRate;

use crate::descriptors::{CLOCK_ID, CONTROL_INTERFACE, Descriptors, STREAMING_INTERFACE};
use crate::stream::{INITIAL_RATE, SUPPORTED_RATES, StreamFormat, WIRE_FORMATS};
use crate::wire::UrbStatus;

// bmRequestType 的字段
const REQUEST_DIRECTION_IN: u8 = 0x80;
const REQUEST_TYPE_SHIFT: u8 = 5;
const REQUEST_TYPE_MASK: u8 = 0x03;
const REQUEST_TYPE_STANDARD: u8 = 0;
const REQUEST_TYPE_CLASS: u8 = 1;
const RECIPIENT_MASK: u8 = 0x1F;
const RECIPIENT_INTERFACE: u8 = 1;

// 标准请求
const GET_STATUS: u8 = 0x00;
const CLEAR_FEATURE: u8 = 0x01;
const SET_FEATURE: u8 = 0x03;
const SET_ADDRESS: u8 = 0x05;
const GET_DESCRIPTOR: u8 = 0x06;
const GET_CONFIGURATION: u8 = 0x08;
const SET_CONFIGURATION: u8 = 0x09;
const GET_INTERFACE: u8 = 0x0A;
const SET_INTERFACE: u8 = 0x0B;
const SYNCH_FRAME: u8 = 0x0C;

// GET_DESCRIPTOR 的描述符类型
const DESCRIPTOR_DEVICE: u8 = 0x01;
const DESCRIPTOR_CONFIGURATION: u8 = 0x02;
const DESCRIPTOR_STRING: u8 = 0x03;
const DESCRIPTOR_QUALIFIER: u8 = 0x06;

// 音频类请求与时钟源的控制选择子
const CLASS_CUR: u8 = 0x01;
const CLASS_RANGE: u8 = 0x02;
const CLOCK_SAMPLING_FREQUENCY: u8 = 0x01;
const CLOCK_VALIDITY: u8 = 0x02;
const CLOCK_IS_VALID: u8 = 1;

/// 控制请求的 8 字节 setup 包。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setup {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

impl Setup {
    pub fn parse(raw: &[u8; 8]) -> Self {
        Self {
            request_type: raw[0],
            request: raw[1],
            value: u16::from_le_bytes([raw[2], raw[3]]),
            index: u16::from_le_bytes([raw[4], raw[5]]),
            length: u16::from_le_bytes([raw[6], raw[7]]),
        }
    }

    fn kind(&self) -> u8 {
        (self.request_type >> REQUEST_TYPE_SHIFT) & REQUEST_TYPE_MASK
    }

    fn recipient(&self) -> u8 {
        self.request_type & RECIPIENT_MASK
    }

    fn is_device_to_host(&self) -> bool {
        self.request_type & REQUEST_DIRECTION_IN != 0
    }

    fn value_high(&self) -> u8 {
        self.value.to_be_bytes()[0]
    }

    fn value_low(&self) -> u8 {
        self.value.to_be_bytes()[1]
    }

    fn index_high(&self) -> u8 {
        self.index.to_be_bytes()[0]
    }

    fn index_low(&self) -> u8 {
        self.index.to_be_bytes()[1]
    }
}

/// 流状态的变化。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamChange {
    /// 流开始，或者在流进行中格式变了（例如主机改了采样率）
    Started(StreamFormat),
    Stopped,
}

/// 一次控制请求的处理结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlOutcome {
    pub status: UrbStatus,
    /// 设备发给主机的数据，已按请求的 wLength 截断
    pub data: Vec<u8>,
    pub stream: Option<StreamChange>,
}

/// 一个槽位的音频功能状态。
#[derive(Debug, Clone)]
pub struct Uac2Function {
    direction: SlotDirection,
    descriptors: Descriptors,
    configuration: u8,
    alternate: u8,
    sample_rate: SampleRate,
}

impl Uac2Function {
    pub fn new(direction: SlotDirection, descriptors: Descriptors) -> Self {
        Self {
            direction,
            descriptors,
            configuration: 0,
            alternate: 0,
            sample_rate: INITIAL_RATE,
        }
    }

    pub fn direction(&self) -> SlotDirection {
        self.direction
    }

    /// 当前流的格式；没有在流动时为 None
    pub fn stream_format(&self) -> Option<StreamFormat> {
        let wire = *WIRE_FORMATS.get(usize::from(self.alternate).checked_sub(1)?)?;
        Some(StreamFormat {
            sample_rate: self.sample_rate,
            wire,
        })
    }

    /// 处理一个控制请求。`out_data` 是主机随请求发来的数据（主机到设备方向的请求才有）。
    pub fn control(&mut self, setup: &Setup, out_data: &[u8]) -> ControlOutcome {
        let was_streaming = self.stream_format();
        let reply = match setup.kind() {
            REQUEST_TYPE_STANDARD => self.standard_request(setup),
            REQUEST_TYPE_CLASS => self.class_request(setup, out_data),
            _ => None,
        };
        let now_streaming = self.stream_format();
        let stream = match (was_streaming, now_streaming) {
            (before, Some(after)) if before != Some(after) => Some(StreamChange::Started(after)),
            (Some(_), None) => Some(StreamChange::Stopped),
            _ => None,
        };
        match reply {
            Some(mut data) => {
                data.truncate(usize::from(setup.length));
                ControlOutcome {
                    status: UrbStatus::Ok,
                    data,
                    stream,
                }
            }
            None => {
                tracing::debug!(?setup, "control request stalled");
                ControlOutcome {
                    status: UrbStatus::Stall,
                    data: Vec::new(),
                    stream,
                }
            }
        }
    }

    /// 返回 None 表示不支持，应答 STALL。
    fn standard_request(&mut self, setup: &Setup) -> Option<Vec<u8>> {
        match setup.request {
            GET_STATUS => Some(vec![0, 0]),
            CLEAR_FEATURE | SET_FEATURE | SET_ADDRESS => Some(Vec::new()),
            GET_DESCRIPTOR => match setup.value_high() {
                DESCRIPTOR_DEVICE => Some(self.descriptors.device.clone()),
                DESCRIPTOR_CONFIGURATION => Some(self.descriptors.configuration.clone()),
                DESCRIPTOR_STRING => self.descriptors.string(setup.value_low()),
                DESCRIPTOR_QUALIFIER => Some(self.descriptors.qualifier.clone()),
                _ => None,
            },
            GET_CONFIGURATION => Some(vec![self.configuration]),
            SET_CONFIGURATION => {
                self.configuration = setup.value_low();
                self.alternate = 0;
                Some(Vec::new())
            }
            GET_INTERFACE => match setup.index_low() {
                STREAMING_INTERFACE => Some(vec![self.alternate]),
                CONTROL_INTERFACE => Some(vec![0]),
                _ => None,
            },
            SET_INTERFACE => self.set_interface(setup.index_low(), setup.value_low()),
            SYNCH_FRAME => Some(vec![0, 0]),
            _ => None,
        }
    }

    fn set_interface(&mut self, interface: u8, alternate: u8) -> Option<Vec<u8>> {
        match interface {
            CONTROL_INTERFACE if alternate == 0 => Some(Vec::new()),
            STREAMING_INTERFACE if usize::from(alternate) <= WIRE_FORMATS.len() => {
                self.alternate = alternate;
                Some(Vec::new())
            }
            _ => None,
        }
    }

    /// 音频类请求：只支持发给音频控制接口里时钟源的请求（采样率与时钟有效性）。
    fn class_request(&mut self, setup: &Setup, out_data: &[u8]) -> Option<Vec<u8>> {
        let to_clock = setup.recipient() == RECIPIENT_INTERFACE
            && setup.index_low() == CONTROL_INTERFACE
            && setup.index_high() == CLOCK_ID;
        if !to_clock {
            return None;
        }
        match (setup.value_high(), setup.request, setup.is_device_to_host()) {
            (CLOCK_SAMPLING_FREQUENCY, CLASS_CUR, true) => Some(self.sample_rate.hz().to_le_bytes().to_vec()),
            (CLOCK_SAMPLING_FREQUENCY, CLASS_RANGE, true) => Some(sample_rate_ranges()),
            (CLOCK_SAMPLING_FREQUENCY, CLASS_CUR, false) => {
                let requested = u32::from_le_bytes(*out_data.first_chunk::<4>()?);
                let rate = SUPPORTED_RATES
                    .contains(&requested)
                    .then(|| SampleRate::new(requested).ok())
                    .flatten()?;
                self.sample_rate = rate;
                Some(Vec::new())
            }
            (CLOCK_VALIDITY, CLASS_CUR, true) => Some(vec![CLOCK_IS_VALID]),
            _ => None,
        }
    }
}

/// RANGE 应答：区间个数，然后每个区间是最小值、最大值、步长（离散值的步长为 0）。
fn sample_rate_ranges() -> Vec<u8> {
    let mut out = Vec::new();
    let count = u16::try_from(SUPPORTED_RATES.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&count.to_le_bytes());
    for rate in SUPPORTED_RATES {
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::IdentityProfile;
    use aem_protocol::ids::{SlotId, SlotNumber};
    use aem_protocol::message::SampleFormat;

    fn function(direction: SlotDirection) -> Uac2Function {
        let slot = SlotId {
            direction,
            number: SlotNumber::new(1).unwrap(),
        };
        let identity = IdentityProfile::Development.identity(slot).unwrap();
        Uac2Function::new(direction, Descriptors::build(&identity))
    }

    fn setup(request_type: u8, request: u8, value: u16, index: u16, length: u16) -> Setup {
        Setup {
            request_type,
            request,
            value,
            index,
            length,
        }
    }

    const STANDARD_IN: u8 = 0x80;
    const STANDARD_INTERFACE_OUT: u8 = 0x01;
    const CLASS_INTERFACE_IN: u8 = 0xA1;
    const CLASS_INTERFACE_OUT: u8 = 0x21;
    const CLOCK_INDEX: u16 = (CLOCK_ID as u16) << 8;

    #[test]
    fn returns_descriptors_truncated_to_the_requested_length() {
        let mut function = function(SlotDirection::In);
        let outcome = function.control(&setup(STANDARD_IN, GET_DESCRIPTOR, 0x0100, 0, 64), &[]);
        assert_eq!((outcome.status, outcome.data.len()), (UrbStatus::Ok, 18));
        let outcome = function.control(&setup(STANDARD_IN, GET_DESCRIPTOR, 0x0200, 0, 9), &[]);
        assert_eq!(outcome.data.len(), 9);
    }

    #[test]
    fn stalls_unknown_requests() {
        let mut function = function(SlotDirection::In);
        let outcome = function.control(&setup(STANDARD_IN, GET_DESCRIPTOR, 0x0900, 0, 64), &[]);
        assert_eq!(outcome.status, UrbStatus::Stall);
        assert!(outcome.data.is_empty());
    }

    #[test]
    fn reports_and_sets_the_sample_rate() {
        let mut function = function(SlotDirection::Out);
        let get = setup(CLASS_INTERFACE_IN, CLASS_CUR, 0x0100, CLOCK_INDEX, 4);
        assert_eq!(function.control(&get, &[]).data, 48_000u32.to_le_bytes().to_vec());

        let set = setup(CLASS_INTERFACE_OUT, CLASS_CUR, 0x0100, CLOCK_INDEX, 4);
        assert_eq!(function.control(&set, &96_000u32.to_le_bytes()).status, UrbStatus::Ok);
        assert_eq!(function.control(&get, &[]).data, 96_000u32.to_le_bytes().to_vec());

        assert_eq!(
            function.control(&set, &12_345u32.to_le_bytes()).status,
            UrbStatus::Stall
        );
        assert_eq!(function.control(&set, &[0, 1]).status, UrbStatus::Stall);
    }

    #[test]
    fn lists_all_supported_rates() {
        let mut function = function(SlotDirection::Out);
        let range = setup(CLASS_INTERFACE_IN, CLASS_RANGE, 0x0100, CLOCK_INDEX, 1024);
        let data = function.control(&range, &[]).data;
        assert_eq!(data.len(), 2 + SUPPORTED_RATES.len() * 12);
        assert_eq!(u16::from_le_bytes([data[0], data[1]]), 10);
    }

    #[test]
    fn reports_stream_start_format_change_and_stop() {
        let mut function = function(SlotDirection::Out);
        let float_alt = 4;
        let start = setup(
            STANDARD_INTERFACE_OUT,
            SET_INTERFACE,
            float_alt,
            u16::from(STREAMING_INTERFACE),
            0,
        );
        let Some(StreamChange::Started(format)) = function.control(&start, &[]).stream else {
            unreachable!("expected the stream to start")
        };
        assert_eq!(format.wire.sample_format, SampleFormat::Float32);
        assert_eq!(format.sample_rate.hz(), 48_000);

        let set_rate = setup(CLASS_INTERFACE_OUT, CLASS_CUR, 0x0100, CLOCK_INDEX, 4);
        let Some(StreamChange::Started(format)) = function.control(&set_rate, &44_100u32.to_le_bytes()).stream else {
            unreachable!("expected a format change")
        };
        assert_eq!(format.sample_rate.hz(), 44_100);

        let stop = setup(
            STANDARD_INTERFACE_OUT,
            SET_INTERFACE,
            0,
            u16::from(STREAMING_INTERFACE),
            0,
        );
        assert_eq!(function.control(&stop, &[]).stream, Some(StreamChange::Stopped));
        assert_eq!(function.control(&stop, &[]).stream, None);
    }

    #[test]
    fn rejects_unknown_alternate_settings() {
        let mut function = function(SlotDirection::In);
        let bad = setup(
            STANDARD_INTERFACE_OUT,
            SET_INTERFACE,
            6,
            u16::from(STREAMING_INTERFACE),
            0,
        );
        assert_eq!(function.control(&bad, &[]).status, UrbStatus::Stall);
        assert_eq!(function.stream_format(), None);
    }
}
