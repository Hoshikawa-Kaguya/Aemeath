//! 一个被导入槽位的流状态，只在调度线程中使用。
//!
//! - 输入槽位（应用往里播放）：等时 OUT URB 一提交就解码进缓冲，但每个包要等时间线走过它所属的微帧，
//!   再多等 [`RENDER_PRIME_MICROFRAMES`]，才"放出"给混音读取。这样延迟由时间线决定，与 URB 的大小
//!   （Windows 实测每个 URB 10 ms）无关。
//! - 输出槽位（OBS 等从里面录）：混音每 1 ms 写入一块；等时 IN URB 到期时按节拍取出、编码、应答。
//!
//! 采样率与引擎不同时走固定比率重采样（比率是精确值，不做漂移校正）：输入方向在放进缓冲前转成引擎采样率，
//! 放出的帧数按引擎采样率换算；输出方向在放进缓冲前转成槽位采样率。
//! 导入、流开始这类低频事件中允许分配内存；逐块、逐 URB 的路径复用缓冲（写线程把写完的缓冲还回来），不申请也不释放内存。

use std::collections::VecDeque;
use std::io;

use aemeath_dsp::codec;
use aemeath_dsp::resample::{FixedSide, StereoResampler};
use aemeath_protocol::ids::{SlotDirection, SlotId};
use aemeath_protocol::topology::Resampler;
use aemeath_protocol::units::SampleRate;
use aemeath_usbip::function::StreamChange;
use aemeath_usbip::stream::{CapturePacer, MICROFRAMES_PER_SECOND, StreamFormat};
use aemeath_usbip::wire::{IsoPacket, RetSubmit, Submit, UrbStatus, encode_ret_submit_into};
use aemeath_usbip::writer::ConnectionWriter;

use crate::STEREO;
use crate::fifo::FrameFifo;

/// URB 迟到（预留的时间已经过去）时，从当前时刻往后这么多微帧重新开始（1 ms）
const RESTART_LEAD_MICROFRAMES: i64 = 8;
/// 输入方向额外的放出延迟（1 ms），吸收主机与引擎分块的 ±1 帧差异
pub const RENDER_PRIME_MICROFRAMES: i64 = 8;
/// 输入槽位的 URB 迟到不超过这么多微帧时仍接在原位置（与放出延迟相同，1 ms）
const LATE_TOLERANCE_MICROFRAMES: i64 = RENDER_PRIME_MICROFRAMES;
/// 缓冲容量：最高采样率下 100 ms
const FIFO_CAPACITY_FRAMES: usize = 76_800;
/// URB 最远可以排到当前时刻之后多久（200 ms）。正常约 20 ms（提前约 10 ms 交来、每个 10 ms），
/// 输入方向超过缓冲容量（100 ms）的数据本来就放不下
const MAX_AHEAD_MICROFRAMES: i64 = 1600;
/// 待放出的包数上限：每个 URB 80 个包，留出几个 URB 的余量
const MAX_PENDING_PACKETS: usize = 1024;
/// 复用的包描述缓冲的初始容量：一个 URB 通常 80 个包
const TYPICAL_ISO_PACKETS: usize = 128;
/// 重采样每块的时长：1 ms（FFT 的分块由预设决定）
const RESAMPLE_CHUNKS_PER_SECOND: u32 = 1000;
/// 输出方向预先放进缓冲的静音，在一块重采样的时长之外再留的余量（1 ms）
const CAPTURE_PREFILL_MARGIN_PER_SECOND: u32 = 1000;

/// 构建一个重采样器需要的全部信息，在辅助线程上用。
#[derive(Debug, Clone, Copy)]
pub struct ResamplerSpec {
    pub slot: SlotId,
    /// 请求的序号：等待构建期间格式又变了，旧结果靠它识别并丢弃
    pub generation: u64,
    pub quality: Resampler,
    pub input_rate: SampleRate,
    pub output_rate: SampleRate,
}

/// 重采样器和它在逐块路径上用的缓冲：全部在辅助线程准备好，调度线程只做交接，不分配内存。
#[derive(Debug)]
pub struct SlotResampler {
    generation: u64,
    resampler: StereoResampler,
    /// 等待凑够一块的输入
    input: Vec<f32>,
    output: Vec<f32>,
    /// 输入方向：成块造成的额外放出延迟（微帧）
    lead: i64,
    /// 输出方向：预先放进缓冲的静音（输入方向为空）
    prefill: Vec<f32>,
}

impl SlotResampler {
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

impl ResamplerSpec {
    /// 构建。失败时记录日志并返回 None，这个槽位保持静音。
    pub fn build(self) -> Option<SlotResampler> {
        let chunk_frames = (self.input_rate.hz() / RESAMPLE_CHUNKS_PER_SECOND).max(1) as usize;
        let resampler = StereoResampler::new(
            self.quality,
            self.input_rate,
            self.output_rate,
            chunk_frames,
            FixedSide::Input,
        )
        .inspect_err(|error| {
            tracing::error!(slot = %self.slot, %error, "cannot create the slot resampler; the slot is silent");
        })
        .ok()?;
        let block_frames = resampler.input_frames_next();
        // 成批量按最大单次产出算：FFT 把分块取整后，一批可能比输入分块对应的帧数多
        let max_output = resampler.output_frames_max();
        // 一个 URB 的 10 ms 和一块的输入都要放得下，逐块路径不再增长
        let urb_frames = self.input_rate.hz() as usize / 100;
        let input = Vec::with_capacity((block_frames * 2 + urb_frames) * STEREO);
        let output = Vec::with_capacity(max_output * 2 * STEREO);
        let burst_microframes = max_output as u64 * MICROFRAMES_PER_SECOND;
        let lead = i64::try_from(burst_microframes.div_ceil(u64::from(self.output_rate.hz()))).unwrap_or(i64::MAX);
        let prefill_frames = match self.slot.direction {
            SlotDirection::In => 0,
            SlotDirection::Out => max_output + (self.output_rate.hz() / CAPTURE_PREFILL_MARGIN_PER_SECOND) as usize,
        };
        Some(SlotResampler {
            generation: self.generation,
            resampler,
            input,
            output,
            lead,
            prefill: vec![0.0; prefill_frames * STEREO],
        })
    }
}

/// 交给辅助线程的工作：释放换下来的重采样器、构建新的。
#[derive(Debug, Default)]
pub struct ResamplerWork {
    pub retired: Option<SlotResampler>,
    pub spec: Option<ResamplerSpec>,
}

/// 一个 URB 在时间线上的位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reservation {
    pub start: i64,
    pub end: i64,
    /// 迟到超过容差、重新开始时，迟到的微帧数
    pub late_by: Option<i64>,
}

impl Reservation {
    /// 这个 URB 结束得比当前时刻晚太多：不是正常的驱动行为，数据要一直留到那时。
    pub fn is_too_far_ahead(&self, now_microframe: i64) -> bool {
        self.end - now_microframe > MAX_AHEAD_MICROFRAMES
    }
}

/// 一个槽位流的问题计数，由调度线程定期汇总进日志。
#[derive(Debug, Default, Clone, Copy)]
pub struct SlotStats {
    /// URB 迟到、时间线预留重新开始的次数
    pub restarts: u64,
    /// 混音要读时数据还没到的帧数
    pub underflow_frames: u64,
    /// 缓冲满而丢弃的帧数
    pub overflow_frames: u64,
}

/// 一个已导入槽位的音频流：按时间线收发等时 URB，在 USB 一侧的格式与引擎之间缓冲和转换。
/// 只在调度线程上使用。
#[derive(Debug)]
pub struct SlotStream {
    slot: SlotId,
    writer: ConnectionWriter,
    format: Option<StreamFormat>,
    engine_rate: Option<SampleRate>,
    next_microframe: Option<i64>,
    fifo: FrameFifo,
    /// 输入方向：(可以放出的微帧, 帧数)
    pending: VecDeque<(i64, usize)>,
    readable_frames: usize,
    pacer: CapturePacer,
    scratch: Vec<f32>,
    /// 编码应答时复用的包描述和音频数据
    reply_packets: Vec<IsoPacket>,
    reply_data: Vec<u8>,
    quality: Resampler,
    /// 采样率与引擎不同、且辅助线程已构建好时存在
    resampler: Option<SlotResampler>,
    /// 最新一次重建请求的序号
    generation: u64,
    /// 输入方向：累计收到的槽位帧数和已安排放出的引擎帧数，用精确比率换算，不累积舍入误差
    render_input_frames: u64,
    render_released_frames: u64,
    pub stats: SlotStats,
}

/// 槽位流用的缓冲（约 600 KB）。分配和释放都要几百微秒（实测最长约 240 µs），调度线程上用辅助线程预先备好的。
pub fn stream_buffer() -> FrameFifo {
    FrameFifo::with_capacity(FIFO_CAPACITY_FRAMES)
}

impl SlotStream {
    /// `fifo` 由 [`stream_buffer`] 创建。
    pub fn new(slot: SlotId, writer: ConnectionWriter, fifo: FrameFifo) -> Self {
        Self {
            slot,
            writer,
            format: None,
            engine_rate: None,
            next_microframe: None,
            fifo,
            pending: VecDeque::with_capacity(MAX_PENDING_PACKETS),
            readable_frames: 0,
            pacer: CapturePacer::default(),
            scratch: Vec::new(),
            reply_packets: Vec::with_capacity(TYPICAL_ISO_PACKETS),
            reply_data: Vec::new(),
            quality: Resampler::default(),
            resampler: None,
            generation: 0,
            render_input_frames: 0,
            render_released_frames: 0,
            stats: SlotStats::default(),
        }
    }

    pub fn slot(&self) -> SlotId {
        self.slot
    }

    pub fn format(&self) -> Option<StreamFormat> {
        self.format
    }

    /// 把应答交给写线程，不阻塞。`submit` 是它对应的 URB，由写线程释放。
    pub fn send(&self, bytes: Vec<u8>, submit: Option<Submit>) -> io::Result<()> {
        self.writer.send(bytes, submit)
    }

    /// 断开这条连接：读线程随后退出，服务端释放槽位。
    pub fn abandon(&self) {
        self.writer.abandon();
    }

    /// 流开始或停止。返回要交给辅助线程的重采样器工作。
    pub fn apply_change(&mut self, change: StreamChange) -> ResamplerWork {
        self.format = match change {
            StreamChange::Started(format) => Some(format),
            StreamChange::Stopped => None,
        };
        self.next_microframe = None;
        self.fifo.clear();
        self.pending.clear();
        self.readable_frames = 0;
        self.pacer.reset();
        self.rebuild_resampler()
    }

    /// 换拓扑时调用：引擎采样率或这个槽位的重采样算法变化时重建重采样器。
    pub fn set_processing(&mut self, rate: Option<SampleRate>, quality: Resampler) -> ResamplerWork {
        if self.engine_rate == rate && self.quality == quality {
            return ResamplerWork::default();
        }
        self.engine_rate = rate;
        self.quality = quality;
        self.fifo.clear();
        self.pending.clear();
        self.readable_frames = 0;
        self.rebuild_resampler()
    }

    /// 换下旧的重采样器，算出新的构建参数。真正的构建在辅助线程（[`crate::resampler_worker`]），
    /// 结果到来之前这个槽位是静音的，只有几毫秒，发生在流开始或换拓扑时。
    fn rebuild_resampler(&mut self) -> ResamplerWork {
        self.generation += 1;
        let retired = self.resampler.take();
        self.render_input_frames = 0;
        self.render_released_frames = 0;
        let spec = match (self.format, self.engine_rate) {
            (Some(format), Some(engine_rate)) if format.sample_rate != engine_rate => {
                let (input_rate, output_rate) = match self.slot.direction {
                    SlotDirection::In => (format.sample_rate, engine_rate),
                    SlotDirection::Out => (engine_rate, format.sample_rate),
                };
                Some(ResamplerSpec {
                    slot: self.slot,
                    generation: self.generation,
                    quality: self.quality,
                    input_rate,
                    output_rate,
                })
            }
            _ => None,
        };
        ResamplerWork { retired, spec }
    }

    /// 装上辅助线程构建好的重采样器。已经过期（等待期间格式又变了）时不装，原样还给调用方去释放。
    pub fn install(&mut self, built: SlotResampler) -> Option<SlotResampler> {
        if built.generation != self.generation {
            return Some(built);
        }
        self.resampler = Some(built);
        self.prefill_capture();
        None
    }

    /// 输出方向：重采样成块到达，先垫一段静音，URB 到期时缓冲里才总有数据。
    fn prefill_capture(&mut self) {
        if let Some(resampler) = &self.resampler {
            self.fifo.push(&resampler.prefill);
        }
    }

    /// 流格式与引擎采样率一致、可以直通时返回格式
    fn passthrough_format(&self) -> Option<StreamFormat> {
        self.format
            .filter(|format| Some(format.sample_rate) == self.engine_rate)
    }

    /// 可以收发音频时返回流格式：直通，或者有重采样器
    fn active_format(&self) -> Option<StreamFormat> {
        self.format
            .filter(|format| Some(format.sample_rate) == self.engine_rate || self.resampler.is_some())
    }

    /// 把攒够的输入重采样后放进缓冲。
    fn convert_buffered(&mut self) {
        let Some(state) = &mut self.resampler else {
            return;
        };
        let mut consumed = 0;
        loop {
            let needed = state.resampler.input_frames_next() * STEREO;
            if state.input.len() - consumed < needed {
                break;
            }
            if let Err(error) = state
                .resampler
                .process(&state.input[consumed..consumed + needed], &mut state.output)
            {
                tracing::error!(slot = %self.slot, %error, "slot resampling failed");
            }
            consumed += needed;
        }
        state.input.drain(..consumed);
        self.stats.overflow_frames += self.fifo.push(&state.output) as u64;
        state.output.clear();
    }

    /// 输入方向：重采样 `scratch` 里的一个包，返回这个包对应放出的引擎帧数。
    fn resample_render(&mut self, frames: usize) -> usize {
        let (Some(format), Some(engine_rate)) = (self.format, self.engine_rate) else {
            return 0;
        };
        if let Some(state) = &mut self.resampler {
            state.input.extend_from_slice(&self.scratch);
        }
        self.convert_buffered();
        self.render_input_frames += frames as u64;
        let target = self.render_input_frames * u64::from(engine_rate.hz()) / u64::from(format.sample_rate.hz());
        let released = target - self.render_released_frames;
        self.render_released_frames = target;
        usize::try_from(released).unwrap_or(0)
    }

    /// 在时间线上为一个 URB 预留位置。
    ///
    /// URB 通常提前约 10 ms 到达，紧接上一个 URB。迟到时是否仍接在原位置，按方向不同：
    /// - 输入槽位（主机播放）：数据本来就多等 1 ms 才放出，迟到不超过 [`LATE_TOLERANCE_MICROFRAMES`] 不会造成空档。
    /// - 输出槽位（主机录音）：数据在 URB 到期时才取，只要它覆盖的时段还没结束（每个 URB 约 10 ms），
    ///   接在原位置就不会有任何空档（实测 Windows 曾迟交 2.9 ms）。
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
            // 首个 URB 或迟到重启
            previous => {
                if previous.is_some() {
                    self.stats.restarts += 1;
                }
                // 混音器在这之前写入的帧不会再有 URB 来取，留着就成了永久的延迟
                if self.slot.direction == SlotDirection::Out {
                    self.fifo.clear();
                    self.prefill_capture();
                }
                (
                    now_microframe + RESTART_LEAD_MICROFRAMES,
                    previous.map(|next| now_microframe - next),
                )
            }
        };
        let end = start + span;
        self.next_microframe = Some(end);
        Reservation { start, end, late_by }
    }

    /// 输入方向：接收一个等时 OUT URB 的数据，返回到期时要发出的应答。
    pub fn accept_render(&mut self, submit: &Submit, start_microframe: i64) -> Vec<u8> {
        let interval = i64::from(submit.interval.max(1));
        if let Some(format) = self.active_format() {
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
                let released = if self.resampler.is_some() {
                    self.resample_render(frames)
                } else {
                    self.stats.overflow_frames += self.fifo.push(&self.scratch) as u64;
                    frames
                };
                let lead = self.resampler.as_ref().map_or(0, |state| state.lead);
                let release = start_microframe + (index as i64 + 1) * interval + RENDER_PRIME_MICROFRAMES + lead;
                self.pending.push_back((release, released));
            }
        }

        self.reply_packets.clear();
        self.reply_packets
            .extend(submit.iso_packets.iter().map(|packet| IsoPacket {
                actual_length: packet.length,
                status: UrbStatus::Ok.code(),
                ..*packet
            }));
        let total: u32 = self.reply_packets.iter().map(|packet| packet.length).sum();
        let mut reply = self.writer.buffer();
        encode_ret_submit_into(
            &RetSubmit {
                seq: submit.seq,
                status: UrbStatus::Ok,
                actual_length: total,
                start_frame: submit.start_frame,
                iso_packets: Some(&self.reply_packets),
                error_count: 0,
                data: &[],
            },
            &mut reply,
        );
        reply
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

    /// 输入方向：混音跳过一段时间后调用，丢掉到 `now_microframe` 为止已放出的积压。
    /// 被跳过的块本该取走它们；留着的话之后每块进 1 ms 出 1 ms，多出的延迟一直留到流重新开始。
    pub fn skip_render_backlog(&mut self, now_microframe: i64) {
        self.release(now_microframe);
        self.fifo.skip(self.readable_frames);
        self.readable_frames = 0;
    }

    /// 输入方向：取出已放出的帧，写满 `out`（交错立体声），不足的部分填 0。
    pub fn pop_render(&mut self, out: &mut [f32]) {
        let wanted = out.len() / STEREO;
        let take = wanted.min(self.readable_frames);
        let popped = self.fifo.pop_into(&mut out[..take * STEREO]);
        out[take * STEREO..].fill(0.0);
        self.readable_frames -= popped;
        // 等重采样器构建的几毫秒本来就是静音，不算欠载
        if self.active_format().is_some() {
            self.stats.underflow_frames += (wanted - popped) as u64;
        }
    }

    /// 输出方向：混音写入一块（引擎采样率）。没有在流动时丢弃。
    pub fn push_capture(&mut self, interleaved: &[f32]) {
        if let Some(state) = &mut self.resampler {
            state.input.extend_from_slice(interleaved);
            self.convert_buffered();
        } else if self.passthrough_format().is_some() {
            self.stats.overflow_frames += self.fifo.push(interleaved) as u64;
        }
    }

    /// 输出方向：一个等时 IN URB 到期，按节拍取出数据并编码成应答。
    pub fn complete_capture(&mut self, submit: &Submit) -> Vec<u8> {
        self.reply_data.clear();
        self.reply_packets.clear();
        let format = self.format;
        let streaming = self.active_format().is_some();
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
                if streaming {
                    let popped = self.fifo.pop_into(&mut self.scratch);
                    self.stats.underflow_frames += (frames - popped) as u64;
                }
                let before = self.reply_data.len();
                codec::encode(format.wire.sample_format, &self.scratch, &mut self.reply_data);
                actual = u32::try_from(self.reply_data.len() - before).unwrap_or(0);
            }
            self.reply_packets.push(IsoPacket {
                offset,
                length: packet.length,
                actual_length: actual,
                status: UrbStatus::Ok.code(),
            });
            offset = offset.saturating_add(packet.length);
        }
        let mut reply = self.writer.buffer();
        encode_ret_submit_into(
            &RetSubmit {
                seq: submit.seq,
                status: UrbStatus::Ok,
                actual_length: u32::try_from(self.reply_data.len()).unwrap_or(0),
                start_frame: submit.start_frame,
                iso_packets: Some(&self.reply_packets),
                error_count: 0,
                data: &self.reply_data,
            },
            &mut reply,
        );
        reply
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
    use aemeath_protocol::message::SampleFormat;
    use aemeath_protocol::topology::{FftPreset, PolyPreset};
    use aemeath_usbip::stream::WIRE_FORMATS;
    use aemeath_usbip::wire::Direction;
    use std::net::{TcpListener, TcpStream};

    /// 80 个包、每包 1 个微帧：与 Windows 实测的 URB 相同（10 ms）
    const PACKETS: usize = 80;
    /// `WIRE_FORMATS` 里 Float32 的位置
    const FLOAT32_WIRE: usize = 3;

    fn stream(direction: SlotDirection) -> SlotStream {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let writer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let slot = SlotId {
            direction,
            number: SlotNumber::new(1).unwrap(),
        };
        SlotStream::new(slot, ConnectionWriter::spawn(writer, 0).unwrap().0, stream_buffer())
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

    const ENGINE_HZ: u32 = 48_000;
    const TONE_HZ: f64 = 997.0;
    const AMPLITUDE: f64 = 0.5;
    /// 测试时长（ms），前面的启动段不检查
    const TEST_MS: usize = 2000;
    const SETTLE_MS: usize = 200;

    fn rate(hz: u32) -> SampleRate {
        SampleRate::new(hz).unwrap()
    }

    fn float_format(hz: u32) -> StreamFormat {
        StreamFormat {
            sample_rate: rate(hz),
            wire: WIRE_FORMATS[FLOAT32_WIRE],
        }
    }

    fn tone(position: u64, hz: u32) -> f32 {
        (AMPLITUDE * (std::f64::consts::TAU * TONE_HZ * position as f64 / f64::from(hz)).sin()) as f32
    }

    /// 单声道序列的频率和连续性：相邻样本的差不能超过正弦的最大斜率（留 30% 余量）
    fn assert_clean_tone(samples: &[f32], hz: u32, label: &str) {
        let crossings = samples
            .windows(2)
            .filter(|pair| pair[0] < 0.0 && pair[1] >= 0.0)
            .count();
        let measured = crossings as f64 * f64::from(hz) / samples.len() as f64;
        assert!((measured - TONE_HZ).abs() < 3.0, "{label}: {measured} Hz");
        let limit = (std::f64::consts::TAU * TONE_HZ * AMPLITUDE / f64::from(hz) * 1.3) as f32;
        let worst = samples
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(0.0, f32::max);
        assert!(worst <= limit, "{label}: step {worst} > {limit}");
    }

    /// 同步走完"流开始 → 构建 → 装上"，真实运行时构建在辅助线程
    fn start(stream: &mut SlotStream, hz: u32, quality: Resampler) {
        stream.set_processing(Some(rate(ENGINE_HZ)), quality);
        let work = stream.apply_change(StreamChange::Started(float_format(hz)));
        finish(stream, work);
    }

    fn finish(stream: &mut SlotStream, work: ResamplerWork) {
        let built = work.spec.and_then(ResamplerSpec::build).unwrap();
        assert!(stream.install(built).is_none());
    }

    fn qualities() -> [Resampler; 3] {
        [
            Resampler::default(),
            Resampler::PolyAsync {
                preset: PolyPreset::Fine,
            },
            Resampler::FftSync {
                preset: FftPreset::Balanced,
            },
        ]
    }

    #[test]
    fn render_slots_convert_to_the_engine_rate() {
        for hz in [44_100, 96_000, 192_000] {
            for quality in qualities() {
                let mut stream = stream(SlotDirection::In);
                start(&mut stream, hz, quality);
                assert!(stream.resampler.is_some());

                let mut pacer = CapturePacer::default();
                let mut position = 0u64;
                let mut output = Vec::new();
                let mut block = vec![0.0f32; ENGINE_HZ as usize / 1000 * STEREO];
                let mut underflow_after_settle = 0;
                for ms in 0..TEST_MS {
                    let microframe = (ms * 8) as i64;
                    // 与 Windows 一样，每 10 ms 提前交来一个 URB
                    if ms % 10 == 0 {
                        let mut urb = submit();
                        let mut bytes = Vec::new();
                        let mut offset = 0u32;
                        for packet in &mut urb.iso_packets {
                            let frames = pacer.next_packet_frames(rate(hz), 1);
                            let samples: Vec<f32> = (0..frames)
                                .flat_map(|index| [tone(position + index as u64, hz); STEREO])
                                .collect();
                            position += frames as u64;
                            let before = bytes.len();
                            codec::encode(SampleFormat::Float32, &samples, &mut bytes);
                            packet.offset = offset;
                            packet.length = (bytes.len() - before) as u32;
                            offset = bytes.len() as u32;
                        }
                        urb.out_data = bytes;
                        stream.accept_render(&urb, microframe);
                    }
                    stream.release(microframe);
                    let before = stream.stats.underflow_frames;
                    stream.pop_render(&mut block);
                    if ms >= SETTLE_MS {
                        underflow_after_settle += stream.stats.underflow_frames - before;
                        output.extend(block.iter().step_by(STEREO).copied());
                    }
                }
                let label = format!("{hz} Hz {quality:?}");
                assert_eq!(underflow_after_settle, 0, "{label}");
                assert_eq!(stream.stats.overflow_frames, 0, "{label}");
                assert_clean_tone(&output, ENGINE_HZ, &label);
            }
        }
    }

    #[test]
    fn capture_slots_convert_to_the_device_rate() {
        for hz in [44_100, 96_000, 192_000] {
            for quality in qualities() {
                let mut stream = stream(SlotDirection::Out);
                start(&mut stream, hz, quality);

                let mut urb = submit();
                for packet in &mut urb.iso_packets {
                    packet.length = (hz / 8000 + 1) * 8;
                }
                let block_frames = ENGINE_HZ as usize / 1000;
                let mut position = 0u64;
                let mut output = Vec::new();
                let mut underflow_after_settle = 0;
                for ms in 1..=TEST_MS {
                    let block: Vec<f32> = (0..block_frames)
                        .flat_map(|index| [tone(position + index as u64, ENGINE_HZ); STEREO])
                        .collect();
                    position += block_frames as u64;
                    stream.push_capture(&block);
                    if ms % 10 == 0 {
                        let before = stream.stats.underflow_frames;
                        stream.complete_capture(&urb);
                        if ms > SETTLE_MS {
                            underflow_after_settle += stream.stats.underflow_frames - before;
                            let mut samples = Vec::new();
                            codec::decode(SampleFormat::Float32, &stream.reply_data, &mut samples);
                            output.extend(samples.iter().step_by(STEREO).copied());
                        }
                    }
                }
                let label = format!("{hz} Hz {quality:?}");
                assert_eq!(underflow_after_settle, 0, "{label}");
                assert_clean_tone(&output, hz, &label);
            }
        }
    }

    #[test]
    fn matching_rates_stay_on_the_direct_path() {
        let mut stream = stream(SlotDirection::In);
        stream.set_processing(Some(rate(ENGINE_HZ)), Resampler::default());
        let work = stream.apply_change(StreamChange::Started(float_format(ENGINE_HZ)));
        assert!(work.spec.is_none());
        assert!(stream.resampler.is_none());
    }

    #[test]
    fn changing_the_algorithm_rebuilds_the_resampler() {
        let mut stream = stream(SlotDirection::Out);
        start(&mut stream, 44_100, Resampler::default());
        let prefill = stream.fifo.len();
        let work = stream.set_processing(
            Some(rate(ENGINE_HZ)),
            Resampler::FftSync {
                preset: FftPreset::Balanced,
            },
        );
        assert!(work.retired.is_some(), "the old resampler goes to the worker");
        assert!(stream.resampler.is_none());
        finish(&mut stream, work);
        assert!(stream.fifo.len() > prefill);
    }

    #[test]
    fn a_result_from_an_older_request_is_not_installed() {
        let mut stream = stream(SlotDirection::Out);
        stream.set_processing(Some(rate(ENGINE_HZ)), Resampler::default());
        let first = stream.apply_change(StreamChange::Started(float_format(44_100)));
        let stale = first.spec.and_then(ResamplerSpec::build).unwrap();
        // 等待构建期间采样率又变了
        let second = stream.apply_change(StreamChange::Started(float_format(96_000)));
        assert!(stream.install(stale).is_some());
        assert!(stream.resampler.is_none());
        finish(&mut stream, second);
        assert!(stream.resampler.is_some());
    }

    #[test]
    fn the_slot_is_silent_while_the_resampler_is_being_built() {
        let mut stream = stream(SlotDirection::In);
        stream.set_processing(Some(rate(ENGINE_HZ)), Resampler::default());
        let _pending = stream.apply_change(StreamChange::Started(float_format(44_100)));
        let mut block = [1.0f32; 96];
        stream.pop_render(&mut block);
        assert!(block.iter().all(|sample| *sample == 0.0));
        assert_eq!(
            stream.stats.underflow_frames, 0,
            "waiting for the build is not an underflow"
        );
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
    fn urbs_queued_too_far_ahead_are_flagged() {
        let mut stream = stream(SlotDirection::In);
        // 正常节奏：每 10 ms 一个，提前约一个 URB
        for now in 0..20 {
            assert!(!stream.reserve(&submit(), now * 80).is_too_far_ahead(now * 80));
        }
        // 一口气交来几十个，排到 200 ms 以后
        let flooded = (0..30).any(|_| stream.reserve(&submit(), 1600).is_too_far_ahead(1600));
        assert!(flooded);

        let mut stream = self::stream(SlotDirection::In);
        let slow = Submit {
            interval: 100,
            ..submit()
        };
        assert!(stream.reserve(&slow, 0).is_too_far_ahead(0));
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
    fn restarting_a_capture_slot_drops_the_frames_no_urb_will_collect() {
        let mut stream = stream(SlotDirection::Out);
        stream.reserve(&submit(), 0);
        stream.fifo.push(&[0.5; 960]);
        stream.reserve(&submit(), 400);
        assert!(stream.fifo.is_empty());
    }

    #[test]
    fn the_first_capture_urb_drops_what_the_mixer_wrote_before_it() {
        let mut stream = stream(SlotDirection::Out);
        stream.fifo.push(&[0.5; 960]);
        stream.reserve(&submit(), 0);
        assert!(stream.fifo.is_empty());
    }

    #[test]
    fn skipping_ahead_drops_the_released_render_backlog() {
        let mut stream = stream(SlotDirection::In);
        // 20 ms 的积压已经到了放出时刻，另有 1 ms 还没到
        stream.fifo.push(&[0.5; 1920 * STEREO]);
        stream.pending.push_back((80, 960));
        stream.pending.push_back((160, 960));
        stream.fifo.push(&[0.25; 48 * STEREO]);
        stream.pending.push_back((400, 48));

        stream.skip_render_backlog(240);
        assert_eq!(stream.fifo.len(), 48, "only the frames not yet due are kept");
        assert_eq!(stream.readable_frames, 0);
        assert_eq!(stream.pending.len(), 1);
    }

    #[test]
    fn restarting_a_render_slot_keeps_its_data() {
        let mut stream = stream(SlotDirection::In);
        stream.reserve(&submit(), 0);
        stream.fifo.push(&[0.5; 960]);
        stream.reserve(&submit(), 400);
        assert_eq!(stream.fifo.len(), 480);
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
