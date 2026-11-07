//! 一个被导入槽位的流状态，只在调度线程中使用。
//!
//! - 输入槽位（应用往里播放）：等时 OUT URB 一提交就解码进缓冲，但每个包要等时间线走过它所属的微帧，
//!   再多等 [`RENDER_PRIME_MICROFRAMES`]，才"放出"给混音读取。这样延迟由时间线决定，与 URB 的大小
//!   （Windows 实测每个 URB 10 ms）无关。
//! - 输出槽位（OBS 等从里面录）：混音每 1 ms 写入一块；等时 IN URB 到期时按节拍取出、编码、应答。
//!
//! 采样率与引擎不同时输出静音；固定比率重采样在 M1 第 5 项加入。
//! 导入、流开始这类低频事件中允许分配内存；逐块、逐 URB 的路径只在应答编码时分配（与 S4 相同，以后改用缓冲池）。

use std::collections::VecDeque;
use std::io::{self, Write};
use std::net::TcpStream;

use aemeath_dsp::codec;
use aemeath_protocol::ids::{SlotDirection, SlotId};
use aemeath_protocol::units::SampleRate;
use aemeath_usbip::function::StreamChange;
use aemeath_usbip::stream::{CapturePacer, StreamFormat};
use aemeath_usbip::wire::{IsoPacket, RetSubmit, Submit, UrbStatus};

use crate::STEREO;
use crate::fifo::FrameFifo;

/// URB 迟到（预留的时间已经过去）时，从当前时刻往后这么多微帧重新开始（与探针相同，1 ms）
const RESTART_LEAD_MICROFRAMES: i64 = 8;
/// 输入方向额外的放出延迟（1 ms），吸收主机与引擎分块的 ±1 帧差异
pub const RENDER_PRIME_MICROFRAMES: i64 = 8;
/// 输入槽位的 URB 迟到不超过这么多微帧时仍接在原位置（与放出延迟相同，1 ms）
const LATE_TOLERANCE_MICROFRAMES: i64 = RENDER_PRIME_MICROFRAMES;
/// 缓冲容量：最高采样率下 100 ms
const FIFO_CAPACITY_FRAMES: usize = 76_800;
/// 待放出的包数上限：每个 URB 80 个包，留出几个 URB 的余量
const MAX_PENDING_PACKETS: usize = 1024;

/// 一个 URB 在时间线上的位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reservation {
    pub start: i64,
    pub end: i64,
    /// 迟到超过容差、重新开始时，迟到的微帧数
    pub late_by: Option<i64>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SlotStats {
    /// URB 迟到、时间线预留重新开始的次数
    pub restarts: u64,
    /// 混音要读时数据还没到的帧数
    pub underflow_frames: u64,
    /// 缓冲满而丢弃的帧数
    pub overflow_frames: u64,
}

#[derive(Debug)]
pub struct SlotStream {
    slot: SlotId,
    writer: TcpStream,
    format: Option<StreamFormat>,
    engine_rate: Option<SampleRate>,
    next_microframe: Option<i64>,
    fifo: FrameFifo,
    /// 输入方向：(可以放出的微帧, 帧数)
    pending: VecDeque<(i64, usize)>,
    readable_frames: usize,
    pacer: CapturePacer,
    scratch: Vec<f32>,
    pub stats: SlotStats,
}

impl SlotStream {
    pub fn new(slot: SlotId, writer: TcpStream) -> Self {
        Self {
            slot,
            writer,
            format: None,
            engine_rate: None,
            next_microframe: None,
            fifo: FrameFifo::with_capacity(FIFO_CAPACITY_FRAMES),
            pending: VecDeque::with_capacity(MAX_PENDING_PACKETS),
            readable_frames: 0,
            pacer: CapturePacer::default(),
            scratch: Vec::new(),
            stats: SlotStats::default(),
        }
    }

    pub fn slot(&self) -> SlotId {
        self.slot
    }

    pub fn format(&self) -> Option<StreamFormat> {
        self.format
    }

    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.writer.write_all(bytes)
    }

    pub fn apply_change(&mut self, change: StreamChange) {
        self.format = match change {
            StreamChange::Started(format) => Some(format),
            StreamChange::Stopped => None,
        };
        self.next_microframe = None;
        self.fifo.clear();
        self.pending.clear();
        self.readable_frames = 0;
        self.pacer.reset();
    }

    /// 引擎采样率变化（换拓扑）时调用。
    pub fn set_engine_rate(&mut self, rate: Option<SampleRate>) {
        if self.engine_rate != rate {
            self.engine_rate = rate;
            self.fifo.clear();
            self.pending.clear();
            self.readable_frames = 0;
        }
    }

    /// 流格式与引擎采样率一致、可以直通时返回格式
    fn passthrough_format(&self) -> Option<StreamFormat> {
        self.format
            .filter(|format| Some(format.sample_rate) == self.engine_rate)
    }

    /// 在时间线上为一个 URB 预留位置。
    ///
    /// URB 通常提前约 10 ms 到达，紧接上一个 URB。迟到时是否仍接在原位置，按方向不同：
    /// - 输入槽位（主机播放）：数据本来就多等 1 ms 才放出，迟到不超过 [`LATE_TOLERANCE_MICROFRAMES`] 不会造成空档。
    /// - 输出槽位（主机录音）：数据在 URB 到期时才取，只要它覆盖的时段还没结束（每个 URB 约 10 ms），
    ///   接在原位置就不会有任何空档（2026-11-07 实测 Windows 曾迟交 2.9 ms）。
    ///
    /// 超出容差时从当前时刻往后重新开始，并在结果中报告迟到的微帧数。
    pub fn reserve(&mut self, submit: &Submit, now_microframe: i64) -> Reservation {
        let interval = i64::from(submit.interval.max(1));
        let packets = i64::try_from(submit.iso_packets.len()).unwrap_or(i64::MAX);
        let span = packets * interval;
        let keeps_position = |next: i64| match self.slot.direction {
            SlotDirection::In => next > now_microframe - LATE_TOLERANCE_MICROFRAMES,
            SlotDirection::Out => next + span > now_microframe,
        };
        let (start, late_by) = match self.next_microframe {
            Some(next) if keeps_position(next) => (next, None),
            Some(next) => {
                self.stats.restarts += 1;
                (now_microframe + RESTART_LEAD_MICROFRAMES, Some(now_microframe - next))
            }
            None => (now_microframe + RESTART_LEAD_MICROFRAMES, None),
        };
        let end = start + span;
        self.next_microframe = Some(end);
        Reservation { start, end, late_by }
    }

    /// 输入方向：接收一个等时 OUT URB 的数据，返回到期时要发出的应答。
    pub fn accept_render(&mut self, submit: &Submit, start_microframe: i64) -> Vec<u8> {
        let interval = i64::from(submit.interval.max(1));
        if let Some(format) = self.passthrough_format() {
            let bytes_per_frame = format.bytes_per_frame();
            for (index, packet) in submit.iso_packets.iter().enumerate() {
                let range = packet_range(packet, submit.out_data.len());
                let data = &submit.out_data[range];
                self.scratch.clear();
                codec::decode(format.wire.sample_format, data, &mut self.scratch);
                let frames = data.len() / bytes_per_frame;
                if self.pending.len() == MAX_PENDING_PACKETS {
                    self.stats.overflow_frames += frames as u64;
                    continue;
                }
                self.stats.overflow_frames += self.fifo.push(&self.scratch) as u64;
                let release = start_microframe + (index as i64 + 1) * interval + RENDER_PRIME_MICROFRAMES;
                self.pending.push_back((release, frames));
            }
        }

        let packets: Vec<IsoPacket> = submit
            .iso_packets
            .iter()
            .map(|packet| IsoPacket {
                actual_length: packet.length,
                status: UrbStatus::Ok.code(),
                ..*packet
            })
            .collect();
        let total: u32 = packets.iter().map(|packet| packet.length).sum();
        aemeath_usbip::wire::encode_ret_submit(&RetSubmit {
            seq: submit.seq,
            status: UrbStatus::Ok,
            actual_length: total,
            start_frame: submit.start_frame,
            iso_packets: Some(&packets),
            error_count: 0,
            data: &[],
        })
    }

    /// 输入方向：放出时间线已经走过的包。
    pub fn release(&mut self, now_microframe: i64) {
        while let Some(&(release, frames)) = self.pending.front() {
            if release > now_microframe {
                break;
            }
            self.pending.pop_front();
            self.readable_frames += frames;
        }
    }

    /// 输入方向：取出已放出的帧，写满 `out`（交错立体声），不足的部分填 0。
    pub fn pop_render(&mut self, out: &mut [f32]) {
        let wanted = out.len() / STEREO;
        let take = wanted.min(self.readable_frames);
        let popped = self.fifo.pop_into(&mut out[..take * STEREO]);
        out[take * STEREO..].fill(0.0);
        self.readable_frames -= popped;
        if self.format.is_some() {
            self.stats.underflow_frames += (wanted - popped) as u64;
        }
    }

    /// 输出方向：混音写入一块。没有在流动或采样率不同时丢弃。
    pub fn push_capture(&mut self, interleaved: &[f32]) {
        if self.passthrough_format().is_some() {
            self.stats.overflow_frames += self.fifo.push(interleaved) as u64;
        }
    }

    /// 输出方向：一个等时 IN URB 到期，按节拍取出数据并编码成应答。
    pub fn complete_capture(&mut self, submit: &Submit) -> Vec<u8> {
        let mut data = Vec::new();
        let mut packets = Vec::with_capacity(submit.iso_packets.len());
        let format = self.passthrough_format().or(self.format);
        let mut offset = 0u32;
        for packet in &submit.iso_packets {
            let mut actual = 0u32;
            if let Some(format) = format {
                let bytes_per_frame = format.bytes_per_frame();
                let frames = self
                    .pacer
                    .next_packet_frames(format.sample_rate, submit.interval)
                    .min(usize::try_from(packet.length).unwrap_or(0) / bytes_per_frame);
                self.scratch.clear();
                self.scratch.resize(frames * STEREO, 0.0);
                if self.passthrough_format().is_some() {
                    let popped = self.fifo.pop_into(&mut self.scratch);
                    self.stats.underflow_frames += (frames - popped) as u64;
                }
                let before = data.len();
                codec::encode(format.wire.sample_format, &self.scratch, &mut data);
                actual = u32::try_from(data.len() - before).unwrap_or(0);
            }
            packets.push(IsoPacket {
                offset,
                length: packet.length,
                actual_length: actual,
                status: UrbStatus::Ok.code(),
            });
            offset = offset.saturating_add(packet.length);
        }
        aemeath_usbip::wire::encode_ret_submit(&RetSubmit {
            seq: submit.seq,
            status: UrbStatus::Ok,
            actual_length: u32::try_from(data.len()).unwrap_or(0),
            start_frame: submit.start_frame,
            iso_packets: Some(&packets),
            error_count: 0,
            data: &data,
        })
    }

    pub fn direction(&self) -> SlotDirection {
        self.slot.direction
    }
}

/// 包在缓冲中的范围；越界的部分截掉（解码器已校验过包描述不越界，这里只是兜底）。
fn packet_range(packet: &IsoPacket, buffer_len: usize) -> std::ops::Range<usize> {
    let start = usize::try_from(packet.offset).unwrap_or(usize::MAX).min(buffer_len);
    let end = start
        .saturating_add(usize::try_from(packet.length).unwrap_or(0))
        .min(buffer_len);
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;
    use aemeath_protocol::ids::SlotNumber;
    use aemeath_usbip::wire::Direction;
    use std::net::TcpListener;

    /// 80 个包、每包 1 个微帧：与 Windows 实测的 URB 相同（10 ms）
    const PACKETS: usize = 80;

    fn stream(direction: SlotDirection) -> SlotStream {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let writer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let slot = SlotId {
            direction,
            number: SlotNumber::new(1).unwrap(),
        };
        SlotStream::new(slot, writer)
    }

    fn submit() -> Submit {
        Submit {
            seq: 1,
            direction: Direction::In,
            endpoint: 2,
            transfer_flags: 0,
            transfer_len: 0,
            start_frame: 0,
            interval: 1,
            setup: [0; 8],
            out_data: Vec::new(),
            iso_packets: vec![IsoPacket::default(); PACKETS],
        }
    }

    #[test]
    fn consecutive_urbs_follow_each_other() {
        let mut stream = stream(SlotDirection::Out);
        let first = stream.reserve(&submit(), 0);
        assert_eq!((first.start, first.end), (8, 88));
        let second = stream.reserve(&submit(), 10);
        assert_eq!((second.start, second.end, second.late_by), (88, 168, None));
    }

    #[test]
    fn capture_slots_keep_their_position_while_the_urb_has_not_ended() {
        let mut stream = stream(SlotDirection::Out);
        stream.reserve(&submit(), 0);
        // 下一个 URB 应从 88 开始，迟到 23 个微帧（2.9 ms）才到：仍在它覆盖的时段内，接在原位置
        let late = stream.reserve(&submit(), 111);
        assert_eq!((late.start, late.late_by), (168 - 80, None));
        assert_eq!(stream.stats.restarts, 0);
        // 到达时它覆盖的时段已经结束：只能重新开始
        let too_late = stream.reserve(&submit(), 400);
        assert_eq!((too_late.start, too_late.late_by), (408, Some(400 - 168)));
    }

    #[test]
    fn render_slots_tolerate_only_one_millisecond() {
        let mut stream = stream(SlotDirection::In);
        stream.reserve(&submit(), 0);
        assert_eq!(stream.reserve(&submit(), 95).late_by, None, "7 microframes late");
        let late = stream.reserve(&submit(), 168 + 20);
        assert_eq!(late.late_by, Some(20));
        assert_eq!(stream.stats.restarts, 1);
    }
}
