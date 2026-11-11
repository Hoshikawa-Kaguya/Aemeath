//! USB/IP 协议 v1.1.1 的报文解析与编码（字段均为大端序）。
//!
//! 只处理字节，不做 IO，便于单元测试和模糊测试。这是对外的攻击面：本机任何进程都能连上服务端端口，
//! 所以所有长度和包数都有上限，越界一律返回 [`WireError`]，调用方应断开连接。
//!
//! 报文布局依据 Linux usbip 协议文档，并与已知能配合 usbip-win2 工作的 Virtual-Cables（BSD-2）交叉核对过，
//! 并在 Windows 上与 usbip-win2 实际联调通过。

use std::fmt;

use thiserror::Error;

/// 协议版本 1.1.1，usbip-win2 只接受这个版本
pub const USBIP_VERSION: u16 = 0x0111;

const OP_REQ_DEVLIST: u16 = 0x8005;
const OP_REP_DEVLIST: u16 = 0x0005;
const OP_REQ_IMPORT: u16 = 0x8003;
const OP_REP_IMPORT: u16 = 0x0003;

/// 操作阶段的请求头：版本、操作码、状态
pub const OP_HEADER_LEN: usize = 8;
/// busid 字段的固定长度，不足部分补零
pub const BUSID_LEN: usize = 32;
const PATH_LEN: usize = 256;
/// 设备路径的前缀；USB/IP 要求填写，Windows 端只用于显示
const PATH_PREFIX: &str = "/sys/devices/platform/aemeath/";

const CMD_SUBMIT: u32 = 1;
const CMD_UNLINK: u32 = 2;
const RET_SUBMIT: u32 = 3;
const RET_UNLINK: u32 = 4;

/// URB 阶段每个命令和应答的固定头长度
pub const URB_HEADER_LEN: usize = 48;
const ISO_DESCRIPTOR_LEN: usize = 16;
/// `number_of_packets` 取这个值表示不是等时传输
const NOT_ISO: u32 = 0xffff_ffff;
const DIRECTION_OUT: u32 = 0;
const DIRECTION_IN: u32 = 1;
/// USB 端点号的上限（0–15）
const MAX_ENDPOINT: u32 = 15;
/// 高速设备
const SPEED_HIGH: u32 = 3;
const BUS_NUMBER: u32 = 1;

/// 单个 URB 的数据长度上限：等时包最大 1024 字节 × 包数上限。控制传输最多 64 KB，也在此范围内。
pub const MAX_TRANSFER_LEN: u32 = 1024 * 1024;
/// 单个 URB 的等时包数上限。Windows 实测每个 URB 最多几十个包。
pub const MAX_ISO_PACKETS: u32 = 1024;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WireError {
    #[error("unsupported USB/IP version {0:#06x}")]
    Version(u16),
    #[error("unknown operation {0:#06x}")]
    UnknownOperation(u16),
    #[error("invalid bus ID")]
    InvalidBusId,
    #[error("unknown URB command {0}")]
    UnknownCommand(u32),
    #[error("invalid direction {0}")]
    InvalidDirection(u32),
    #[error("invalid endpoint {0}")]
    InvalidEndpoint(u32),
    #[error("transfer of {0} bytes exceeds the limit")]
    TransferTooLarge(u32),
    #[error("{0} isochronous packets exceed the limit")]
    TooManyPackets(u32),
    #[error("an isochronous packet lies outside the transfer buffer")]
    PacketOutOfBounds,
}

/// 操作阶段的请求。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpRequest {
    /// 列出可导入的设备；后面没有更多数据
    DevList,
    /// 导入一台设备；后面跟 [`BUSID_LEN`] 字节的 busid
    Import,
}

/// 解析连接建立后的第一个请求头，判断主机要列出设备还是导入设备。
/// 版本不对或操作码不认识时报错，调用方应断开连接。
pub fn parse_op_header(header: &[u8; OP_HEADER_LEN]) -> Result<OpRequest, WireError> {
    let mut reader = Reader::new(header);
    let version = reader.u16()?;
    let code = reader.u16()?;
    if version != USBIP_VERSION {
        return Err(WireError::Version(version));
    }
    match code {
        OP_REQ_DEVLIST => Ok(OpRequest::DevList),
        OP_REQ_IMPORT => Ok(OpRequest::Import),
        other => Err(WireError::UnknownOperation(other)),
    }
}

/// USB/IP 的 busid，例如 `1-3`。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BusId(String);

impl BusId {
    /// busid 只允许可见 ASCII，长度 1–31（留一个字节给结尾的 NUL）。
    pub fn new(id: &str) -> Result<Self, WireError> {
        let valid = !id.is_empty() && id.len() < BUSID_LEN && id.bytes().all(|byte| byte.is_ascii_graphic());
        if valid {
            Ok(Self(id.to_owned()))
        } else {
            Err(WireError::InvalidBusId)
        }
    }

    /// 从报文里补零的定长字段读出 busid；没有结尾的零或不是合法 busid 时报错。
    pub fn parse(raw: &[u8; BUSID_LEN]) -> Result<Self, WireError> {
        let end = raw.iter().position(|byte| *byte == 0).ok_or(WireError::InvalidBusId)?;
        let text = std::str::from_utf8(&raw[..end]).map_err(|_| WireError::InvalidBusId)?;
        Self::new(text)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn to_fixed<const N: usize>(&self) -> [u8; N] {
        let mut fixed = [0u8; N];
        let len = self.0.len().min(N - 1);
        fixed[..len].copy_from_slice(&self.0.as_bytes()[..len]);
        fixed
    }
}

impl fmt::Display for BusId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// 操作应答的状态（Linux usbip 的 `ST_*`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpStatus {
    Ok,
    /// 设备忙：已经被导入（用于区分残留的后台重连）
    DeviceBusy,
    /// 没有这台设备，或者它没有被导出（槽位没有被占用）
    NoDevice,
}

impl OpStatus {
    fn code(self) -> u32 {
        match self {
            Self::Ok => 0,
            Self::DeviceBusy => 2,
            Self::NoDevice => 4,
        }
    }
}

/// 一台可导入设备的静态信息（USB/IP 的 `usbip_usb_device` 记录及其接口列表）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRecord {
    pub busid: BusId,
    pub device_number: u32,
    pub vendor_id: u16,
    pub product_id: u16,
    pub device_release: u16,
    pub class: ClassTriple,
    pub interfaces: Vec<ClassTriple>,
}

/// 类、子类、协议。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassTriple {
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
}

const CONFIGURATION_VALUE: u8 = 1;
const NUM_CONFIGURATIONS: u8 = 1;

fn put_device_record(out: &mut Vec<u8>, record: &DeviceRecord) {
    let path = format!("{PATH_PREFIX}{}", record.busid);
    let mut path_bytes = [0u8; PATH_LEN];
    let path_len = path.len().min(PATH_LEN - 1);
    path_bytes[..path_len].copy_from_slice(&path.as_bytes()[..path_len]);
    out.extend_from_slice(&path_bytes);
    out.extend_from_slice(&record.busid.to_fixed::<BUSID_LEN>());
    put_u32(out, BUS_NUMBER);
    put_u32(out, record.device_number);
    put_u32(out, SPEED_HIGH);
    put_u16(out, record.vendor_id);
    put_u16(out, record.product_id);
    put_u16(out, record.device_release);
    out.extend_from_slice(&[record.class.class, record.class.subclass, record.class.protocol]);
    out.extend_from_slice(&[CONFIGURATION_VALUE, NUM_CONFIGURATIONS]);
    out.push(u8::try_from(record.interfaces.len()).unwrap_or(u8::MAX));
}

/// 编码导入请求的应答。只有成功时才附带设备记录，失败时只有状态码。
pub fn encode_import_reply(status: OpStatus, record: Option<&DeviceRecord>) -> Vec<u8> {
    let mut out = Vec::new();
    put_u16(&mut out, USBIP_VERSION);
    put_u16(&mut out, OP_REP_IMPORT);
    put_u32(&mut out, status.code());
    if let (OpStatus::Ok, Some(record)) = (status, record) {
        put_device_record(&mut out, record);
    }
    out
}

/// 编码设备列表的应答：每台设备一条记录，后面跟它的各个接口。
pub fn encode_devlist_reply(records: &[DeviceRecord]) -> Vec<u8> {
    let mut out = Vec::new();
    put_u16(&mut out, USBIP_VERSION);
    put_u16(&mut out, OP_REP_DEVLIST);
    put_u32(&mut out, OpStatus::Ok.code());
    put_u32(&mut out, u32::try_from(records.len()).unwrap_or(u32::MAX));
    for record in records {
        put_device_record(&mut out, record);
        for interface in &record.interfaces {
            // 每个接口 4 字节：类、子类、协议、填充
            out.extend_from_slice(&[interface.class, interface.subclass, interface.protocol, 0]);
        }
    }
    out
}

/// 传输方向，从主机的视角：Out 是主机发给设备，In 是设备发给主机。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Out,
    In,
}

/// 一个等时包的描述。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IsoPacket {
    pub offset: u32,
    pub length: u32,
    pub actual_length: u32,
    pub status: i32,
}

/// `CMD_SUBMIT`：主机提交的一个 URB。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submit {
    pub seq: u32,
    pub direction: Direction,
    pub endpoint: u8,
    pub transfer_flags: u32,
    pub transfer_len: u32,
    pub start_frame: u32,
    /// 等时包间隔，单位是微帧
    pub interval: u32,
    pub setup: [u8; 8],
    /// 主机发来的数据（只有 Out 方向有）
    pub out_data: Vec<u8>,
    /// 等时包描述；为空表示不是等时传输
    pub iso_packets: Vec<IsoPacket>,
}

impl Submit {
    pub fn is_iso(&self) -> bool {
        !self.iso_packets.is_empty()
    }
}

/// URB 阶段主机发来的一条命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrbCommand {
    /// 主机提交一个 URB，等设备完成后应答
    Submit(Submit),
    /// 主机取消一个尚未完成的 URB
    Unlink { seq: u32, target_seq: u32 },
}

/// URB 阶段的增量解码器：可以按任意大小分块喂入字节。
#[derive(Debug, Default)]
pub struct UrbDecoder {
    buffer: Vec<u8>,
}

impl UrbDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// 追加从连接读到的字节，之后用 [`Self::next_command`] 取出完整的命令。
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// 取出下一个完整的命令；数据还不够时返回 `Ok(None)`。返回错误后连接已不可信，应断开。
    pub fn next_command(&mut self) -> Result<Option<UrbCommand>, WireError> {
        let Some(header) = self.buffer.first_chunk::<URB_HEADER_LEN>() else {
            return Ok(None);
        };
        let header = *header;
        let mut reader = Reader::new(&header);
        let command = reader.u32()?;
        let seq = reader.u32()?;
        let _devid = reader.u32()?;
        let direction = reader.u32()?;
        let endpoint = reader.u32()?;

        match command {
            CMD_UNLINK => {
                let target_seq = reader.u32()?;
                self.buffer.drain(..URB_HEADER_LEN);
                Ok(Some(UrbCommand::Unlink { seq, target_seq }))
            }
            CMD_SUBMIT => self.next_submit(&mut reader, seq, direction, endpoint),
            other => Err(WireError::UnknownCommand(other)),
        }
    }

    /// 解析 SUBMIT 的剩余部分（数据和等时包描述）。数据还没收全时返回 `Ok(None)`，缓冲保持不动等下次再试。
    fn next_submit(
        &mut self,
        header: &mut Reader<'_>,
        seq: u32,
        direction: u32,
        endpoint: u32,
    ) -> Result<Option<UrbCommand>, WireError> {
        let direction = match direction {
            DIRECTION_OUT => Direction::Out,
            DIRECTION_IN => Direction::In,
            other => return Err(WireError::InvalidDirection(other)),
        };
        if endpoint > MAX_ENDPOINT {
            return Err(WireError::InvalidEndpoint(endpoint));
        }
        let transfer_flags = header.u32()?;
        let transfer_len = header.u32()?;
        let start_frame = header.u32()?;
        let packet_count = header.u32()?;
        let interval = header.u32()?;
        let setup = header.array::<8>()?;

        if transfer_len > MAX_TRANSFER_LEN {
            return Err(WireError::TransferTooLarge(transfer_len));
        }
        let packet_count = match packet_count {
            NOT_ISO | 0 => 0,
            count if count > MAX_ISO_PACKETS => return Err(WireError::TooManyPackets(count)),
            count => count,
        };

        // 上面的上限保证这些长度都不会溢出
        let data_len = if direction == Direction::Out {
            transfer_len as usize
        } else {
            0
        };
        let total = URB_HEADER_LEN + data_len + packet_count as usize * ISO_DESCRIPTOR_LEN;
        if self.buffer.len() < total {
            return Ok(None);
        }

        let body = &self.buffer[URB_HEADER_LEN..total];
        let out_data = body[..data_len].to_vec();
        let mut reader = Reader::new(&body[data_len..]);
        let mut iso_packets = Vec::with_capacity(packet_count as usize);
        for _ in 0..packet_count {
            let packet = IsoPacket {
                offset: reader.u32()?,
                length: reader.u32()?,
                actual_length: reader.u32()?,
                status: reader.i32()?,
            };
            let end = packet.offset.checked_add(packet.length);
            if end.is_none_or(|end| end > transfer_len) {
                return Err(WireError::PacketOutOfBounds);
            }
            iso_packets.push(packet);
        }
        self.buffer.drain(..total);

        Ok(Some(UrbCommand::Submit(Submit {
            seq,
            direction,
            endpoint: endpoint as u8,
            transfer_flags,
            transfer_len,
            start_frame,
            interval,
            setup,
            out_data,
            iso_packets,
        })))
    }
}

/// URB 的完成状态（Linux errno 的负值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrbStatus {
    Ok,
    /// 端点 STALL：不支持的控制请求
    Stall,
    /// URB 已被取消
    ConnectionReset,
}

impl UrbStatus {
    /// 写进应答的状态码：沿用 Linux 的负 errno，usbip-win2 按它判断 URB 的结果。
    pub fn code(self) -> i32 {
        match self {
            Self::Ok => 0,
            Self::Stall => -32,
            Self::ConnectionReset => -104,
        }
    }
}

/// `RET_SUBMIT` 的内容。等时 IN 的 `data` 是按各包实际长度紧密拼接后的数据。
#[derive(Debug, Clone, Copy)]
pub struct RetSubmit<'a> {
    pub seq: u32,
    pub status: UrbStatus,
    pub actual_length: u32,
    pub start_frame: u32,
    /// 等时传输时为各包的结果；非等时传输为 None
    pub iso_packets: Option<&'a [IsoPacket]>,
    pub error_count: u32,
    pub data: &'a [u8],
}

/// 编码一个 URB 的完成应答。实时路径请用 [`encode_ret_submit_into`]，可以复用缓冲、不分配内存。
pub fn encode_ret_submit(reply: &RetSubmit<'_>) -> Vec<u8> {
    let mut out = Vec::new();
    encode_ret_submit_into(reply, &mut out);
    out
}

/// 把应答追加到 `out`：实时路径用它复用缓冲，不申请内存。
pub fn encode_ret_submit_into(reply: &RetSubmit<'_>, out: &mut Vec<u8>) {
    let iso = reply.iso_packets.unwrap_or_default();
    out.reserve(URB_HEADER_LEN + reply.data.len() + iso.len() * ISO_DESCRIPTOR_LEN);
    put_u32(out, RET_SUBMIT);
    put_u32(out, reply.seq);
    // devid、direction、ep 在应答中不使用，填 0
    put_u32(out, 0);
    put_u32(out, 0);
    put_u32(out, 0);
    put_i32(out, reply.status.code());
    let carries_data = reply.status == UrbStatus::Ok || reply.iso_packets.is_some();
    put_u32(out, if carries_data { reply.actual_length } else { 0 });
    match reply.iso_packets {
        Some(packets) => {
            put_u32(out, reply.start_frame);
            put_u32(out, u32::try_from(packets.len()).unwrap_or(u32::MAX));
        }
        None => {
            put_u32(out, 0);
            put_u32(out, NOT_ISO);
        }
    }
    put_u32(out, reply.error_count);
    // setup 字段在应答中不使用
    out.extend_from_slice(&[0u8; 8]);
    if carries_data {
        out.extend_from_slice(reply.data);
    }
    for packet in iso {
        put_u32(out, packet.offset);
        put_u32(out, packet.length);
        put_u32(out, packet.actual_length);
        put_i32(out, packet.status);
    }
}

/// 编码取消请求的应答。`status` 说明被取消的 URB 是否还没完成。
pub fn encode_ret_unlink(seq: u32, status: UrbStatus) -> Vec<u8> {
    let mut out = Vec::with_capacity(URB_HEADER_LEN);
    put_u32(&mut out, RET_UNLINK);
    put_u32(&mut out, seq);
    put_u32(&mut out, 0);
    put_u32(&mut out, 0);
    put_u32(&mut out, 0);
    put_i32(&mut out, status.code());
    out.resize(URB_HEADER_LEN, 0);
    out
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn put_i32(out: &mut Vec<u8>, value: i32) {
    out.extend_from_slice(&value.to_be_bytes());
}

/// 按大端序顺序读取，越界返回错误而不是 panic。
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], WireError> {
        let chunk = self
            .bytes
            .get(self.pos..)
            .and_then(|rest| rest.first_chunk::<N>())
            .ok_or(WireError::PacketOutOfBounds)?;
        self.pos += N;
        Ok(*chunk)
    }

    fn u16(&mut self) -> Result<u16, WireError> {
        self.array().map(u16::from_be_bytes)
    }

    fn u32(&mut self) -> Result<u32, WireError> {
        self.array().map(u32::from_be_bytes)
    }

    fn i32(&mut self) -> Result<i32, WireError> {
        self.array().map(i32::from_be_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn submit_header(seq: u32, direction: u32, endpoint: u32, len: u32, packets: u32) -> Vec<u8> {
        let mut out = Vec::new();
        for value in [CMD_SUBMIT, seq, 0, direction, endpoint, 0, len, 0, packets, 1] {
            put_u32(&mut out, value);
        }
        out.extend_from_slice(&[0u8; 8]);
        out
    }

    fn iso_descriptor(offset: u32, length: u32) -> Vec<u8> {
        let mut out = Vec::new();
        for value in [offset, length, 0, 0] {
            put_u32(&mut out, value);
        }
        out
    }

    #[test]
    fn parses_op_headers() {
        assert_eq!(
            parse_op_header(&[0x01, 0x11, 0x80, 0x03, 0, 0, 0, 0]),
            Ok(OpRequest::Import)
        );
        assert_eq!(
            parse_op_header(&[0x01, 0x11, 0x80, 0x05, 0, 0, 0, 0]),
            Ok(OpRequest::DevList)
        );
        assert_eq!(
            parse_op_header(&[0x01, 0x10, 0x80, 0x03, 0, 0, 0, 0]),
            Err(WireError::Version(0x0110))
        );
        assert_eq!(
            parse_op_header(&[0x01, 0x11, 0x80, 0x07, 0, 0, 0, 0]),
            Err(WireError::UnknownOperation(0x8007))
        );
    }

    #[test]
    fn parses_bus_ids() {
        let mut raw = [0u8; BUSID_LEN];
        raw[..4].copy_from_slice(b"1-12");
        assert_eq!(BusId::parse(&raw).unwrap().as_str(), "1-12");

        assert_eq!(BusId::parse(&[0u8; BUSID_LEN]), Err(WireError::InvalidBusId));
        assert_eq!(BusId::parse(&[b'1'; BUSID_LEN]), Err(WireError::InvalidBusId));
        let mut control_char = [0u8; BUSID_LEN];
        control_char[..2].copy_from_slice(b"1\n");
        assert_eq!(BusId::parse(&control_char), Err(WireError::InvalidBusId));
    }

    #[test]
    fn decodes_control_and_iso_submits_fed_in_pieces() {
        let mut bytes = submit_header(1, DIRECTION_IN, 0, 18, NOT_ISO);
        let mut iso = submit_header(2, DIRECTION_OUT, 1, 8, 2);
        iso.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        iso.extend(iso_descriptor(0, 4));
        iso.extend(iso_descriptor(4, 4));
        bytes.extend(iso);

        let mut decoder = UrbDecoder::new();
        let mut commands = Vec::new();
        for chunk in bytes.chunks(7) {
            decoder.push(chunk);
            while let Some(command) = decoder.next_command().unwrap() {
                commands.push(command);
            }
        }

        assert_eq!(commands.len(), 2);
        let UrbCommand::Submit(control) = &commands[0] else {
            unreachable!("expected a submit")
        };
        assert_eq!(
            (control.seq, control.direction, control.is_iso()),
            (1, Direction::In, false)
        );
        assert!(control.out_data.is_empty());
        let UrbCommand::Submit(iso) = &commands[1] else {
            unreachable!("expected a submit")
        };
        assert_eq!(iso.out_data, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(iso.iso_packets.len(), 2);
        assert_eq!(iso.iso_packets[1].offset, 4);
    }

    #[test]
    fn decodes_unlink() {
        let mut bytes = Vec::new();
        for value in [CMD_UNLINK, 9, 0, 0, 0, 5] {
            put_u32(&mut bytes, value);
        }
        bytes.resize(URB_HEADER_LEN, 0);
        let mut decoder = UrbDecoder::new();
        decoder.push(&bytes);
        assert_eq!(
            decoder.next_command(),
            Ok(Some(UrbCommand::Unlink { seq: 9, target_seq: 5 }))
        );
    }

    #[test]
    fn rejects_oversized_transfers_before_buffering_them() {
        let mut decoder = UrbDecoder::new();
        decoder.push(&submit_header(1, DIRECTION_OUT, 1, MAX_TRANSFER_LEN + 1, NOT_ISO));
        assert_eq!(
            decoder.next_command(),
            Err(WireError::TransferTooLarge(MAX_TRANSFER_LEN + 1))
        );
    }

    #[test]
    fn rejects_too_many_packets() {
        let mut decoder = UrbDecoder::new();
        decoder.push(&submit_header(1, DIRECTION_IN, 2, 0, MAX_ISO_PACKETS + 1));
        assert_eq!(
            decoder.next_command(),
            Err(WireError::TooManyPackets(MAX_ISO_PACKETS + 1))
        );
    }

    #[test]
    fn rejects_packets_outside_the_buffer() {
        let mut bytes = submit_header(1, DIRECTION_IN, 2, 8, 1);
        bytes.extend(iso_descriptor(u32::MAX, 2));
        let mut decoder = UrbDecoder::new();
        decoder.push(&bytes);
        assert_eq!(decoder.next_command(), Err(WireError::PacketOutOfBounds));
    }

    #[test]
    fn rejects_bad_direction_endpoint_and_command() {
        let mut decoder = UrbDecoder::new();
        decoder.push(&submit_header(1, 7, 0, 0, NOT_ISO));
        assert_eq!(decoder.next_command(), Err(WireError::InvalidDirection(7)));

        let mut decoder = UrbDecoder::new();
        decoder.push(&submit_header(1, DIRECTION_IN, 16, 0, NOT_ISO));
        assert_eq!(decoder.next_command(), Err(WireError::InvalidEndpoint(16)));

        let mut decoder = UrbDecoder::new();
        let mut bytes = submit_header(1, DIRECTION_IN, 0, 0, NOT_ISO);
        bytes[3] = 9;
        decoder.push(&bytes);
        assert_eq!(decoder.next_command(), Err(WireError::UnknownCommand(9)));
    }

    #[test]
    fn encodes_ret_submit_for_control_and_iso() {
        let control = encode_ret_submit(&RetSubmit {
            seq: 3,
            status: UrbStatus::Ok,
            actual_length: 2,
            start_frame: 0,
            iso_packets: None,
            error_count: 0,
            data: &[0xAA, 0xBB],
        });
        assert_eq!(control.len(), URB_HEADER_LEN + 2);
        assert_eq!(&control[32..36], &NOT_ISO.to_be_bytes());

        let packets = [IsoPacket {
            offset: 0,
            length: 4,
            actual_length: 4,
            status: 0,
        }];
        let iso = encode_ret_submit(&RetSubmit {
            seq: 4,
            status: UrbStatus::Ok,
            actual_length: 4,
            start_frame: 7,
            iso_packets: Some(&packets),
            error_count: 0,
            data: &[1, 2, 3, 4],
        });
        assert_eq!(iso.len(), URB_HEADER_LEN + 4 + ISO_DESCRIPTOR_LEN);
        assert_eq!(&iso[28..32], &7u32.to_be_bytes());
    }

    #[test]
    fn stalled_control_reply_carries_no_data() {
        let reply = encode_ret_submit(&RetSubmit {
            seq: 3,
            status: UrbStatus::Stall,
            actual_length: 18,
            start_frame: 0,
            iso_packets: None,
            error_count: 0,
            data: &[1; 18],
        });
        assert_eq!(reply.len(), URB_HEADER_LEN);
        assert_eq!(&reply[20..24], &(-32i32).to_be_bytes());
        assert_eq!(&reply[24..28], &0u32.to_be_bytes());
    }

    #[test]
    fn import_reply_has_the_documented_size() {
        let record = DeviceRecord {
            busid: BusId::new("1-1").unwrap(),
            device_number: 1,
            vendor_id: 0x1209,
            product_id: 0x0001,
            device_release: 0x0203,
            class: ClassTriple {
                class: 0xEF,
                subclass: 0x02,
                protocol: 0x01,
            },
            interfaces: vec![],
        };
        // 8 字节头 + 312 字节的 usbip_usb_device
        assert_eq!(encode_import_reply(OpStatus::Ok, Some(&record)).len(), 8 + 312);
        assert_eq!(encode_import_reply(OpStatus::DeviceBusy, Some(&record)).len(), 8);
        assert_eq!(
            &encode_import_reply(OpStatus::DeviceBusy, None)[4..8],
            &2u32.to_be_bytes()
        );
    }
}
