//! 应用源（GLOSSARY：应用源；M3 计划 2.2）：按可执行文件的绝对路径寻找进程，用进程回环采集它的声音。
//!
//! 每个应用源一个寻找线程，每 [`SCAN_INTERVAL`] 取一次进程快照，找出这个 exe 的"根进程"（父进程不是同一个 exe 的进程），
//! 每个根进程开一个采集线程（进程回环 `include_tree = true`，包含子进程）。多个根进程的声音由引擎端口求和。
//!
//! 实测（2026-11-08，Windows 11 25H2）：进程回环支持事件驱动，每 10 ms 一个包；程序不出声、停流、甚至从没放过声音时
//! 也持续送全零包，所以这里没有"无信号"判定，也不需要补静音。
//! 进程回环没有混音格式，由我们指定引擎采样率、f32、立体声，转换由 Windows 完成；每条流仍按 QPC 时钟运行，要经过 ASRC。

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use aemeath_protocol::ids::ProgramPath;
use aemeath_protocol::message::{DeviceFormat, EndpointStatus, OfflineReason, SampleFormat};
use aemeath_protocol::topology::Resampler;
use aemeath_protocol::units::SampleRate;
use aemeath_sys::com::ComApartment;
use aemeath_sys::process::{ProcessEntry, ProcessExit, process_path, process_snapshot};
use rtrb::{Consumer, Producer};
use wasapi::{AudioClient, Direction, StreamMode, WasapiError};

use crate::bridge::BridgeError;
use crate::capture::CapturePump;
use crate::device::{self, EVENT_WAIT_MS, IoContext, OpenStream};
use crate::format::to_wave_format;
use crate::redetect::RedetectTarget;
use crate::{BridgeHandle, STEREO, stereo_ring};

/// 每个应用源最多同时采集的根进程数。环形缓冲在开始时一次分配好，调度线程不用处理增减。
pub const MAX_ROOTS: usize = 8;
const SCAN_INTERVAL: Duration = Duration::from_secs(2);
/// 寻找线程检查采集线程是否结束的间隔：进程退出后这么久内报告"未运行"
const REAP_INTERVAL: Duration = Duration::from_millis(200);
/// 进程回环流的缓冲（100 ns 单位）：20 ms，两倍于 10 ms 的包间隔
const LOOPBACK_BUFFER_HNS: i64 = 200_000;
const BYTES_PER_FLOAT: usize = 4;

/// 一个应用源的配置。任何一项改变，引擎都会重建它。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppSourceConfig {
    pub program: ProgramPath,
    pub engine_rate: SampleRate,
    pub quality: Resampler,
}

/// 应用源状态的变化。
#[derive(Debug, Clone, PartialEq)]
pub struct AppSourceEvent {
    pub program: ProgramPath,
    pub status: EndpointStatus,
}

/// [`MAX_ROOTS`] 组环形缓冲。
pub fn app_rings(engine_rate: SampleRate) -> (Vec<Producer<f32>>, Vec<Consumer<f32>>) {
    (0..MAX_ROOTS).map(|_| stereo_ring(engine_rate)).unzip()
}

pub fn spawn_app_source(
    config: AppSourceConfig,
    producers: Vec<Producer<f32>>,
    context: IoContext,
) -> io::Result<BridgeHandle> {
    let stop = device::stop_flag();
    let thread_stop = stop.clone();
    let thread = thread::Builder::new()
        .name("aemeath-app-find".to_owned())
        .spawn(move || Finder::new(config, producers, context).run(&thread_stop))?;
    Ok(BridgeHandle::new(stop, thread))
}

/// 根进程：匹配的进程里，父进程不是同一个 exe 的那些（`include_tree` 会带上它们的子进程）。
fn root_processes(matching: &[ProcessEntry]) -> Vec<u32> {
    matching
        .iter()
        .filter(|entry| !matching.iter().any(|other| other.process_id == entry.parent_id))
        .map(|entry| entry.process_id)
        .collect()
}

/// 路径完全匹配 `program` 的进程。先按文件名过滤，只对同名进程查询完整路径。
fn matching_processes(program: &ProgramPath) -> Vec<ProcessEntry> {
    let name = program.file_name();
    let snapshot = match process_snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            tracing::warn!(%error, "cannot take a process snapshot");
            return Vec::new();
        }
    };
    snapshot
        .into_iter()
        .filter(|entry| entry.exe_name.eq_ignore_ascii_case(name))
        .filter(|entry| {
            process_path(entry.process_id)
                .and_then(|path| path.to_str().and_then(|path| ProgramPath::new(path).ok()))
                .is_some_and(|path| path.same_program(program))
        })
        .collect()
}

/// 一个采集线程。线程结束时交回环形缓冲的写入端。
struct RootCapture {
    process_id: u32,
    stop: Arc<AtomicBool>,
    opened: Arc<AtomicBool>,
    thread: JoinHandle<RootEnd>,
}

struct RootEnd {
    producer: Producer<f32>,
    failed: bool,
}

struct Finder {
    config: AppSourceConfig,
    free: Vec<Producer<f32>>,
    context: IoContext,
    target: RedetectTarget,
    roots: Vec<RootCapture>,
    /// 最近一次采集失败、进程却还在：报告离线，下一轮扫描再试
    open_failed: bool,
    reported: Option<EndpointStatus>,
}

impl Finder {
    fn new(config: AppSourceConfig, free: Vec<Producer<f32>>, context: IoContext) -> Self {
        let target = RedetectTarget::Program(config.program.clone());
        Self {
            config,
            free,
            context,
            target,
            roots: Vec::new(),
            open_failed: false,
            reported: None,
        }
    }

    fn report(&mut self, status: EndpointStatus) {
        if self.reported != Some(status) {
            tracing::info!(program = %self.config.program.as_str(), ?status, "app source status");
            self.reported = Some(status);
            (self.context.app_status)(AppSourceEvent {
                program: self.config.program.clone(),
                status,
            });
        }
    }

    fn run(&mut self, stop: &AtomicBool) {
        while !stop.load(Ordering::Relaxed) {
            if !Path::is_file(self.config.program.as_path()) {
                self.stop_all();
                self.report(EndpointStatus::ProgramMissing);
                // 不再定时查找（M3 计划 A3）：等用户"重新检测"；路径改变时引擎会重建这个线程
                self.context.redetect.wait(&self.target, None, stop);
                continue;
            }
            self.scan();
            let next_scan = Instant::now() + SCAN_INTERVAL;
            while Instant::now() < next_scan && !stop.load(Ordering::Relaxed) {
                self.reap();
                self.update_status();
                self.context.redetect.wait(&self.target, Some(REAP_INTERVAL), stop);
            }
        }
        self.stop_all();
    }

    fn scan(&mut self) {
        self.reap();
        let matching = matching_processes(&self.config.program);
        if matching.is_empty() {
            self.open_failed = false;
        }
        for process_id in root_processes(&matching) {
            if self.roots.iter().any(|root| root.process_id == process_id) {
                continue;
            }
            let Some(producer) = self.free.pop() else {
                tracing::warn!(program = %self.config.program.as_str(), "too many instances, ignoring the rest");
                break;
            };
            self.start(process_id, producer);
        }
        self.update_status();
    }

    fn start(&mut self, process_id: u32, producer: Producer<f32>) {
        let stop = device::stop_flag();
        let opened = Arc::new(AtomicBool::new(false));
        let (thread_stop, thread_opened) = (Arc::clone(&stop), Arc::clone(&opened));
        let (program, engine_rate, quality) = (
            self.config.program.clone(),
            self.config.engine_rate,
            self.config.quality,
        );
        let thread = thread::Builder::new()
            .name("aemeath-app-capture".to_owned())
            .spawn(move || {
                let mut producer = producer;
                let result = capture_root(
                    process_id,
                    &program,
                    engine_rate,
                    quality,
                    &mut producer,
                    &thread_stop,
                    &thread_opened,
                );
                if let Err(error) = &result {
                    tracing::warn!(program = %program.as_str(), process_id, %error, "process loopback failed");
                }
                RootEnd {
                    producer,
                    failed: result.is_err(),
                }
            });
        match thread {
            Ok(thread) => self.roots.push(RootCapture {
                process_id,
                stop,
                opened,
                thread,
            }),
            Err(error) => tracing::error!(%error, "cannot start the process loopback thread"),
        }
    }

    /// 收回已结束的采集线程，把它的环形缓冲放回空闲列表。
    fn reap(&mut self) {
        let (finished, running): (Vec<_>, Vec<_>) = std::mem::take(&mut self.roots)
            .into_iter()
            .partition(|root| root.thread.is_finished());
        self.roots = running;
        for root in finished {
            self.collect(root);
        }
    }

    fn collect(&mut self, root: RootCapture) {
        if let Ok(end) = root.thread.join() {
            self.open_failed |= end.failed;
            self.free.push(end.producer);
        }
    }

    fn update_status(&mut self) {
        if self.roots.iter().any(|root| root.opened.load(Ordering::Relaxed)) {
            self.open_failed = false;
            self.report(EndpointStatus::Online {
                format: loopback_format(self.config.engine_rate),
            });
        } else if self.open_failed {
            self.report(EndpointStatus::Offline {
                reason: OfflineReason::WaitingForDevice,
            });
        } else if self.roots.is_empty() {
            self.report(EndpointStatus::NotRunning);
        }
    }

    fn stop_all(&mut self) {
        for root in std::mem::take(&mut self.roots) {
            root.stop.store(true, Ordering::Relaxed);
            self.collect(root);
        }
        self.open_failed = false;
    }
}

fn loopback_format(engine_rate: SampleRate) -> DeviceFormat {
    DeviceFormat {
        sample_rate: engine_rate,
        sample_format: SampleFormat::Float32,
        channels: STEREO as u16,
    }
}

fn capture_root(
    process_id: u32,
    program: &ProgramPath,
    engine_rate: SampleRate,
    quality: Resampler,
    producer: &mut Producer<f32>,
    stop: &AtomicBool,
    opened: &AtomicBool,
) -> Result<(), BridgeError> {
    let _priority = aemeath_sys::thread_priority::promote_current_thread()
        .inspect_err(|error| tracing::warn!(%error, "cannot raise the app capture thread priority"));
    let _apartment = ComApartment::multithreaded().map_err(|error| BridgeError::Device(error.into()))?;
    // 进程已经退出时打不开，视为正常结束
    let Ok(exit) = ProcessExit::open(process_id) else {
        return Ok(());
    };
    let mut client = AudioClient::new_application_loopback_client(process_id, true)?;
    let format = loopback_format(engine_rate);
    client.initialize_client(
        &to_wave_format(format),
        Direction::Capture,
        StreamMode::EventsShared {
            autoconvert: true,
            buffer_duration_hns: LOOPBACK_BUFFER_HNS,
        },
        None,
    )?;
    let event = client.set_get_eventhandle()?;
    let stream = OpenStream {
        client,
        event,
        format,
        block_align: STEREO * BYTES_PER_FLOAT,
        exclusive: false,
    };
    let capture = stream.client.get_audiocaptureclient()?;
    let mut pump = CapturePump::new(&stream, engine_rate, quality, producer, program.as_str())?;
    stream.client.start_stream()?;
    opened.store(true, Ordering::Relaxed);
    tracing::info!(program = %program.as_str(), process_id, "process loopback started");

    loop {
        if stop.load(Ordering::Relaxed) || producer.is_abandoned() || exit.has_exited() {
            let _ = stream.client.stop_stream();
            return Ok(());
        }
        match stream.event.wait_for_event(EVENT_WAIT_MS) {
            Ok(()) => {}
            Err(WasapiError::EventTimeout) => continue,
            Err(error) => return Err(error.into()),
        }
        pump.read_packets(&capture)?;
        pump.deliver(producer)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(process_id: u32, parent_id: u32) -> ProcessEntry {
        ProcessEntry {
            process_id,
            parent_id,
            exe_name: "chrome.exe".to_owned(),
        }
    }

    #[test]
    fn roots_are_the_processes_whose_parent_is_a_different_program() {
        // 100 → 101、102；200 是另一个实例；300 的父进程已经退出
        let matching = [
            entry(100, 1),
            entry(101, 100),
            entry(102, 100),
            entry(200, 7),
            entry(300, 999),
        ];
        assert_eq!(root_processes(&matching), vec![100, 200, 300]);
        assert!(root_processes(&[]).is_empty());
    }
}
