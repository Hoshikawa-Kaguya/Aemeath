//! 主时钟调度线程：唯一的实时线程，统一完成所有槽位的等时 URB，并按时间线每 1 ms 运行一次混音。
//!
//! 每轮循环：处理控制命令 → 处理服务端事件 → 运行到期的混音块 → 完成到期的 URB →
//! 用高精度定时器睡到下一个截止时刻（最多 0.5 ms，以便及时应答控制传输）。
//!
//! 换拓扑时，新混音器先接过旧混音器的当前增益（[`Mixer::inherit`]），旧的送回控制线程释放，
//! 不在本线程中释放大块内存。

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::Duration;

use aemeath_protocol::ids::SlotId;
use aemeath_protocol::message::DeviceFormat;
use aemeath_sys::clock::now_hns;
use aemeath_sys::timer::HighResolutionTimer;
use aemeath_usbip::server::{ConnectionId, ServerEvent};
use aemeath_usbip::stream::{CHANNELS as SLOT_CHANNELS, CapturePacer};
use aemeath_usbip::wire::{Direction, Submit, UrbStatus, encode_ret_unlink};

use crate::mixer::{MAX_BLOCK_FRAMES, Mixer};
use crate::port::PortIssues;
use crate::resampler_worker::ResamplerWorker;
use crate::slot::SlotStream;
use crate::timeline::{MICROFRAMES_PER_MS, Timeline};
use crate::{EngineEvent, STEREO};

const MICROSECONDS_PER_MICROFRAME: i64 = 125;
const HNS_PER_MICROSECOND: i64 = 10;
/// 两轮循环相隔超过这么久时记录警告（正常最多 0.5 ms）
const LOOP_GAP_WARNING_HNS: i64 = 30_000;
/// 没有更早的截止时刻时，最多睡这么久
const MAX_SLEEP_HNS: i64 = 5_000;
/// 混音落后超过这么多微帧（10 ms）时不再逐块追赶，直接跳到当前时刻
const MAX_CATCH_UP_MICROFRAMES: i64 = 80;
/// 外部设备端口的问题最多每隔这么久记一次日志（100 ns 单位，5 s）
const PORT_ISSUE_LOG_INTERVAL_HNS: i64 = 50_000_000;

/// 调度线程单轮耗时的峰值（µs）：调度线程每轮写入，控制线程每秒取走。
#[derive(Debug, Default)]
pub struct RealtimeLoad(AtomicU32);

impl RealtimeLoad {
    fn record(&self, busy_hns: i64) {
        let micros = u32::try_from((busy_hns / HNS_PER_MICROSECOND).max(0)).unwrap_or(u32::MAX);
        self.0.fetch_max(micros, Ordering::Relaxed);
    }

    /// 上次取走以来单轮耗时的峰值占混音块周期（1 ms）的百分比，并清零。
    pub fn take_peak_percent(&self) -> f32 {
        const PERCENT_PER_MICROSECOND: f32 = 100.0 / 1000.0;
        self.0.swap(0, Ordering::Relaxed) as f32 * PERCENT_PER_MICROSECOND
    }
}

/// 控制线程发给调度线程的命令。
#[derive(Debug)]
pub enum Command {
    Apply(Box<Mixer>),
    Shutdown,
}

/// 到期时要发出的应答
#[derive(Debug)]
enum Pending {
    /// 输入槽位的 OUT URB：应答已编好；URB 留到发出时交给写线程释放
    Ready(Vec<u8>, Submit),
    /// 输出槽位的 IN URB：到期时才取数据
    Capture(Submit),
}

/// 调度线程的全部状态。只在调度线程上使用，与其他线程只经通道和原子量往来。
pub struct Scheduler {
    timeline: Arc<Timeline>,
    commands: Receiver<Command>,
    server_events: Receiver<ServerEvent>,
    retired: Sender<Box<Mixer>>,
    events: Sender<EngineEvent>,
    mixer: Option<Box<Mixer>>,
    streams: HashMap<ConnectionId, SlotStream>,
    slots: HashMap<SlotId, ConnectionId>,
    deadlines: BinaryHeap<Reverse<(i64, u64, u32)>>,
    pending: HashMap<(u64, u32), Pending>,
    /// 下一个混音块的起始微帧；没有拓扑时为 None
    next_block: Option<i64>,
    block_pacer: CapturePacer,
    /// 外部设备端口自上次记录以来的问题
    port_issues: PortIssues,
    last_port_log: i64,
    resamplers: ResamplerWorker,
    load: Arc<RealtimeLoad>,
}

impl Scheduler {
    pub fn new(
        timeline: Arc<Timeline>,
        commands: Receiver<Command>,
        server_events: Receiver<ServerEvent>,
        retired: Sender<Box<Mixer>>,
        events: Sender<EngineEvent>,
        resamplers: ResamplerWorker,
        load: Arc<RealtimeLoad>,
    ) -> Self {
        Self {
            timeline,
            commands,
            server_events,
            retired,
            events,
            mixer: None,
            streams: HashMap::new(),
            slots: HashMap::new(),
            deadlines: BinaryHeap::new(),
            pending: HashMap::new(),
            next_block: None,
            block_pacer: CapturePacer::default(),
            port_issues: PortIssues::default(),
            last_port_log: 0,
            resamplers,
            load,
        }
    }

    /// 调度线程的主循环：提升到 MMCSS 优先级，按时间线等到最近的截止时刻，完成到期的 URB、运行混音，
    /// 直到收到停止命令。
    pub fn run(mut self) {
        let priority = aemeath_sys::thread_priority::promote_current_thread();
        if let Err(error) = &priority {
            tracing::warn!(%error, "cannot raise the scheduler thread to MMCSS priority");
        }
        let timer = HighResolutionTimer::new()
            .inspect_err(|error| tracing::warn!(%error, "no high-resolution timer, falling back to sleep"))
            .ok();

        let mut last_iteration = now_hns();
        loop {
            if !self.handle_commands() {
                break;
            }
            self.handle_server_events();
            self.install_built_resamplers();
            let now = now_hns();
            if now - last_iteration > LOOP_GAP_WARNING_HNS {
                tracing::warn!(
                    gap_us = (now - last_iteration) / HNS_PER_MICROSECOND,
                    "scheduler loop was delayed"
                );
            }
            last_iteration = now;
            self.run_blocks(self.timeline.microframe_at(now));
            self.complete_due(now);
            let done = now_hns();
            self.load.record(done - now);
            self.sleep(timer.as_ref(), done);
        }
        tracing::info!("scheduler stopped");
    }

    /// 装上辅助线程构建好的重采样器；连接已经关了或结果已过期的，交回辅助线程释放。
    fn install_built_resamplers(&mut self) {
        while let Some((connection, built)) = self.resamplers.try_receive() {
            let unused = match self.streams.get_mut(&connection) {
                Some(stream) => stream.install(built),
                None => Some(built),
            };
            if let Some(unused) = unused {
                self.resamplers.retire(unused);
            }
        }
    }

    /// 返回 false 表示应退出。
    fn handle_commands(&mut self) -> bool {
        loop {
            match self.commands.try_recv() {
                Ok(Command::Apply(mut mixer)) => {
                    if let Some(old) = self.mixer.as_mut() {
                        mixer.inherit(old);
                    }
                    let rate = mixer.sample_rate();
                    if self.mixer.as_ref().map(|old| old.sample_rate()) != Some(rate) {
                        self.next_block = None;
                        self.block_pacer.reset();
                    }
                    for (connection, stream) in &mut self.streams {
                        let work = stream.set_processing(Some(rate), mixer.slot_resampler(stream.slot()));
                        self.resamplers.submit(*connection, work);
                    }
                    tracing::info!(revision = mixer.revision().0, rate = rate.hz(), "topology applied");
                    if let Some(old) = self.mixer.replace(mixer) {
                        let _ = self.retired.send(old);
                    }
                }
                Ok(Command::Shutdown) | Err(TryRecvError::Disconnected) => return false,
                Err(TryRecvError::Empty) => return true,
            }
        }
    }

    /// 取完 USB/IP 服务端积压的事件：新导入的连接、提交和取消的 URB、断开的连接。不阻塞。
    fn handle_server_events(&mut self) {
        while let Ok(event) = self.server_events.try_recv() {
            match event {
                ServerEvent::Imported {
                    slot,
                    connection,
                    writer,
                } => {
                    let mut stream = SlotStream::new(slot, writer);
                    if let Some(mixer) = &self.mixer {
                        // 流还没开始，这里不会产生构建工作
                        let _ = stream.set_processing(Some(mixer.sample_rate()), mixer.slot_resampler(slot));
                    }
                    self.streams.insert(connection, stream);
                    self.slots.insert(slot, connection);
                    let _ = self.events.send(EngineEvent::SlotConnection { slot, connected: true });
                }
                ServerEvent::Released { slot, connection } => self.drop_connection(connection, slot),
                ServerEvent::Reply { connection, bytes } => self.send(connection, bytes, None),
                ServerEvent::StreamChanged {
                    slot,
                    connection,
                    change,
                } => {
                    if let Some(stream) = self.streams.get_mut(&connection) {
                        let work = stream.apply_change(change);
                        self.resamplers.submit(connection, work);
                        let format = stream.format().map(|format| DeviceFormat {
                            sample_rate: format.sample_rate,
                            sample_format: format.wire.sample_format,
                            channels: u16::from(SLOT_CHANNELS),
                        });
                        let _ = self.events.send(EngineEvent::SlotFormat { slot, format });
                    }
                }
                ServerEvent::IsoSubmit {
                    connection,
                    submit,
                    received_hns,
                    ..
                } => self.schedule(connection, submit, received_hns),
                ServerEvent::Unlink {
                    connection,
                    seq,
                    target_seq,
                } => {
                    let found = self.pending.remove(&(connection.0, target_seq)).is_some();
                    let status = if found {
                        UrbStatus::ConnectionReset
                    } else {
                        UrbStatus::Ok
                    };
                    self.send(connection, encode_ret_unlink(seq, status), None);
                }
            }
        }
    }

    /// 为一个等时 URB 在时间线上排好完成时刻，到期时由 [`Self::complete_due`] 应答。
    fn schedule(&mut self, connection: ConnectionId, submit: Submit, received_hns: i64) {
        let Some(stream) = self.streams.get_mut(&connection) else {
            return;
        };
        let now = now_hns();
        let reservation = stream.reserve(&submit, self.timeline.microframe_at(now));
        if let Some(late_by) = reservation.late_by {
            // 区分是主机提交得晚，还是 URB 在读线程到调度线程之间排队太久
            tracing::warn!(
                slot = %stream.slot(),
                late_us = late_by * MICROSECONDS_PER_MICROFRAME,
                queued_us = (now - received_hns) / HNS_PER_MICROSECOND,
                "isochronous URB arrived late, restarting the slot timeline"
            );
        }
        let (start, end) = (reservation.start, reservation.end);
        let deadline = self.timeline.time_of(end);
        let seq = submit.seq;
        let pending = match submit.direction {
            Direction::Out => Pending::Ready(stream.accept_render(&submit, start), submit),
            Direction::In => Pending::Capture(submit),
        };
        self.pending.insert((connection.0, seq), pending);
        self.deadlines.push(Reverse((deadline, connection.0, seq)));
    }

    /// 运行所有到期的混音块。块 [b, b+8) 在时间线走到 b+8 时处理。
    fn run_blocks(&mut self, now_microframe: i64) {
        let Some(mixer) = self.mixer.as_mut() else {
            return;
        };
        let start = self.next_block.get_or_insert(now_microframe);
        if now_microframe - *start > MAX_CATCH_UP_MICROFRAMES {
            tracing::warn!(behind = now_microframe - *start, "mixer fell behind, skipping ahead");
            *start = now_microframe - MICROFRAMES_PER_MS;
        }
        while *start + MICROFRAMES_PER_MS <= now_microframe {
            let end = *start + MICROFRAMES_PER_MS;
            let frames = self
                .block_pacer
                .next_packet_frames(mixer.sample_rate(), MICROFRAMES_PER_MS as u32)
                .min(MAX_BLOCK_FRAMES);

            mixer.begin_block(frames);
            for index in 0..mixer.slot_inputs().len() {
                let (node, slot) = mixer.slot_inputs()[index];
                let stream = self
                    .slots
                    .get(&slot)
                    .and_then(|connection| self.streams.get_mut(connection));
                if let (Some(stream), Some(buffer)) = (stream, mixer.input_mut(node, frames)) {
                    stream.release(end);
                    stream.pop_render(buffer);
                }
            }
            mixer.pull_ports(frames, &mut self.port_issues);
            mixer.process(frames);
            mixer.push_ports(frames, &mut self.port_issues);
            for index in 0..mixer.slot_outputs().len() {
                let (node, slot) = mixer.slot_outputs()[index];
                let stream = self
                    .slots
                    .get(&slot)
                    .and_then(|connection| self.streams.get_mut(connection));
                if let (Some(stream), Some(buffer)) = (stream, mixer.slot_output(node, frames)) {
                    stream.push_capture(buffer);
                }
            }
            *start = end;
        }
        debug_assert_eq!(STEREO, usize::from(SLOT_CHANNELS));
        self.log_port_issues();
    }

    /// 外部设备端口出了问题时记录日志（限频；正常时什么也不做）。
    fn log_port_issues(&mut self) {
        if self.port_issues == PortIssues::default() {
            return;
        }
        let now = now_hns();
        if now - self.last_port_log < PORT_ISSUE_LOG_INTERVAL_HNS {
            return;
        }
        let issues = std::mem::take(&mut self.port_issues);
        tracing::warn!(
            input_underflows = issues.underflows,
            input_skipped_samples = issues.skipped_samples,
            output_dropped_samples = issues.dropped_samples,
            "external device ports had problems"
        );
        self.last_port_log = now;
    }

    /// 应答所有已到期的 URB。录音方向的数据到这时才从缓冲里取，尽量用上最新的混音结果。
    fn complete_due(&mut self, now: i64) {
        while let Some(Reverse((deadline, connection, seq))) = self.deadlines.peek().copied() {
            if deadline > now {
                break;
            }
            self.deadlines.pop();
            let Some(pending) = self.pending.remove(&(connection, seq)) else {
                continue;
            };
            let connection = ConnectionId(connection);
            let (bytes, submit) = match pending {
                Pending::Ready(bytes, submit) => (bytes, submit),
                Pending::Capture(submit) => match self.streams.get_mut(&connection) {
                    Some(stream) => (stream.complete_capture(&submit), submit),
                    None => continue,
                },
            };
            self.send(connection, bytes, Some(submit));
        }
    }

    /// 睡到下一个 URB 到期或下一个混音块该运行的时刻，最多睡 [`MAX_SLEEP_HNS`]，以便及时处理新事件。
    /// 高精度定时器不可用时退回普通 `sleep`。
    fn sleep(&self, timer: Option<&HighResolutionTimer>, now: i64) {
        let mut wake = now + MAX_SLEEP_HNS;
        if let Some(Reverse((deadline, _, _))) = self.deadlines.peek() {
            wake = wake.min(*deadline);
        }
        if let Some(start) = self.next_block {
            wake = wake.min(self.timeline.time_of(start + MICROFRAMES_PER_MS));
        }
        let duration = wake - now;
        if duration <= 0 {
            return;
        }
        match timer {
            Some(timer) => {
                if timer.sleep(duration).is_err() {
                    std::thread::sleep(hns_to_duration(duration));
                }
            }
            None => std::thread::sleep(hns_to_duration(duration)),
        }
    }

    fn send(&mut self, connection: ConnectionId, bytes: Vec<u8>, submit: Option<Submit>) {
        let Some(stream) = self.streams.get_mut(&connection) else {
            return;
        };
        if let Err(error) = stream.send(bytes, submit) {
            let slot = stream.slot();
            tracing::info!(%slot, %error, "cannot queue the reply, dropping the connection");
            self.drop_connection(connection, slot);
        }
    }

    /// 丢弃一条连接的全部状态；它的重采样器交给辅助线程释放。
    fn drop_connection(&mut self, connection: ConnectionId, slot: SlotId) {
        if let Some(mut stream) = self.streams.remove(&connection) {
            if let Some(resampler) = stream.take_resampler() {
                self.resamplers.retire(resampler);
            }
            let stats = stream.stats;
            tracing::info!(
                %slot,
                restarts = stats.restarts,
                underflow_frames = stats.underflow_frames,
                overflow_frames = stats.overflow_frames,
                "slot stream closed"
            );
            if self.slots.get(&slot) == Some(&connection) {
                self.slots.remove(&slot);
            }
            self.pending.retain(|(owner, _), _| *owner != connection.0);
            let _ = self.events.send(EngineEvent::SlotConnection { slot, connected: false });
        }
    }
}

fn hns_to_duration(hns: i64) -> Duration {
    const NANOS_PER_HNS: u64 = 100;
    Duration::from_nanos(u64::try_from(hns).unwrap_or(0) * NANOS_PER_HNS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn realtime_load_reports_the_peak_and_clears() {
        let load = RealtimeLoad::default();
        load.record(1_200);
        load.record(4_200);
        load.record(-10);
        assert!((load.take_peak_percent() - 42.0).abs() < 1e-3);
        assert_eq!(load.take_peak_percent(), 0.0);
    }
}
