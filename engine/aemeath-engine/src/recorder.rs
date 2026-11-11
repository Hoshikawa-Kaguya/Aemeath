//! 录音节点：实时线程把信号推进无锁环形缓冲，每个录音节点一个非实时的写盘线程取走并写文件。
//!
//! 写盘线程按节点 ID 存在，不随混音器替换，切换拓扑时继续写同一个文件；节点被删除时结束文件。
//! 录音状态只在引擎里：主程序重启时录音不中断，引擎崩溃后不自动续录。
//! 每路一个线程：一块磁盘卡住时不影响其他录音。

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use aemeath_protocol::ids::NodeId;
use aemeath_protocol::message::{RecorderState, RecordingFailure};
use aemeath_protocol::topology::{NodeKind, RecorderSettings, RouteTopology};
use aemeath_protocol::units::SampleRate;
use rtrb::{Consumer, RingBuffer};

use crate::STEREO;
use crate::mixer::Mixer;
use crate::port::Port;
use crate::recording_file::{RecordingFile, WAV_SEGMENT_LIMIT};

/// 环形缓冲能存的秒数（至少 2 s），满了就丢弃并由调度线程计数
const RING_SECONDS: usize = 2;
const WRITER_POLL: Duration = Duration::from_millis(20);
const STATUS_INTERVAL: Duration = Duration::from_secs(1);
const FADE_SECONDS: f64 = 0.010;
/// `ERROR_DISK_FULL` 与 `ERROR_HANDLE_DISK_FULL`
const WINDOWS_DISK_FULL: [i32; 2] = [112, 39];

/// 录音状态的回调（由引擎转成事件）。
pub type RecorderSink = Arc<dyn Fn(NodeId, RecorderState) + Send + Sync>;

enum WriterCommand {
    Start { path: PathBuf, settings: RecorderSettings },
    Stop,
}

struct RecorderHandle {
    commands: Sender<WriterCommand>,
    settings: RecorderSettings,
    thread: Option<JoinHandle<()>>,
}

impl Drop for RecorderHandle {
    /// 通道断开后，写盘线程结束文件并退出。
    fn drop(&mut self) {
        let (closed, _) = mpsc::channel();
        drop(std::mem::replace(&mut self.commands, closed));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// 全部录音节点的写盘线程，按节点 ID 管理。
pub struct Recorders {
    handles: HashMap<NodeId, RecorderHandle>,
    status: RecorderSink,
}

impl Recorders {
    pub fn new(status: RecorderSink) -> Self {
        Self {
            handles: HashMap::new(),
            status,
        }
    }

    /// 为新混音器准备录音节点：新出现的建环形缓冲和写盘线程，还在的沿用，消失的结束文件。
    pub fn prepare(&mut self, topology: &RouteTopology, mixer: &mut Mixer) {
        let rate = topology.engine_sample_rate;
        let mut keep = Vec::new();
        for (index, node) in topology.nodes.iter().enumerate() {
            let NodeKind::Recorder { settings } = node.kind else {
                continue;
            };
            keep.push(node.id);
            if let Some(handle) = self.handles.get_mut(&node.id)
                && mixer.inherits_port(index)
            {
                // 格式设置对下一次录音生效，不影响正在写的文件
                handle.settings = settings;
                continue;
            }
            self.handles.remove(&node.id);
            match self.spawn(node.id, settings, rate, mixer, index) {
                Ok(handle) => {
                    self.handles.insert(node.id, handle);
                }
                Err(error) => tracing::error!(node = node.id.0, %error, "cannot start the recording thread"),
            }
        }
        self.handles.retain(|id, _| keep.contains(id));
    }

    /// 为新出现的录音节点建环形缓冲并启动写盘线程，把生产端交给混音器。
    fn spawn(
        &self,
        node: NodeId,
        settings: RecorderSettings,
        rate: SampleRate,
        mixer: &mut Mixer,
        index: usize,
    ) -> io::Result<RecorderHandle> {
        let samples = rate.hz() as usize * RING_SECONDS * STEREO;
        let (producer, consumer) = RingBuffer::new(samples);
        let active = Arc::new(AtomicBool::new(false));
        let fade = (f64::from(rate.hz()) * FADE_SECONDS).round() as u32;
        mixer.attach_port(index, Port::output(producer, Arc::clone(&active), fade));
        let (commands, receiver) = mpsc::channel();
        let status = Arc::clone(&self.status);
        let thread = thread::Builder::new()
            .name("aemeath-recorder".to_owned())
            .spawn(move || {
                Writer {
                    node,
                    rate,
                    consumer,
                    active,
                    status,
                    file: None,
                }
                .run(&receiver);
            })?;
        Ok(RecorderHandle {
            commands,
            settings,
            thread: Some(thread),
        })
    }

    /// 让这个录音节点开始写 `path`。节点不存在或写盘线程已退出时返回 false。
    pub fn start(&self, node: NodeId, path: PathBuf) -> bool {
        self.handles.get(&node).is_some_and(|handle| {
            handle
                .commands
                .send(WriterCommand::Start {
                    path,
                    settings: handle.settings,
                })
                .is_ok()
        })
    }

    /// 让这个录音节点停止并结束文件。节点不存在或写盘线程已退出时返回 false。
    pub fn stop(&self, node: NodeId) -> bool {
        self.handles
            .get(&node)
            .is_some_and(|handle| handle.commands.send(WriterCommand::Stop).is_ok())
    }

    /// 引擎退出时调用：结束全部写盘线程，正在写的文件都会正常收尾。
    pub fn shutdown(&mut self) {
        self.handles.clear();
    }
}

struct OpenFile {
    file: RecordingFile,
    base: PathBuf,
    last_status: Instant,
}

struct Writer {
    node: NodeId,
    rate: SampleRate,
    consumer: Consumer<f32>,
    active: Arc<AtomicBool>,
    status: RecorderSink,
    file: Option<OpenFile>,
}

/// 把写盘错误归成界面能说明的原因：磁盘满单独提示，其余都是写入失败。
fn failure_reason(error: &io::Error) -> RecordingFailure {
    let disk_full = error.kind() == io::ErrorKind::StorageFull
        || error
            .raw_os_error()
            .is_some_and(|code| WINDOWS_DISK_FULL.contains(&code));
    if disk_full {
        RecordingFailure::DiskFull
    } else {
        RecordingFailure::WriteFailed
    }
}

impl Writer {
    /// 写盘线程的主循环：处理开始、停止命令，定时把缓冲写进文件；命令通道断开时结束文件并退出。
    fn run(&mut self, commands: &Receiver<WriterCommand>) {
        loop {
            match commands.recv_timeout(WRITER_POLL) {
                Ok(WriterCommand::Start { path, settings }) => self.start(&path, settings),
                Ok(WriterCommand::Stop) => self.stop(),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    self.stop();
                    return;
                }
            }
            self.drain();
            self.report_progress();
        }
    }

    fn report(&self, state: RecorderState) {
        (self.status)(self.node, state);
    }

    /// 建文件并开始录音；已在录音时忽略。先丢掉缓冲里开始之前的旧数据。
    fn start(&mut self, path: &Path, settings: RecorderSettings) {
        if self.file.is_some() {
            return;
        }
        match RecordingFile::open(path, settings, self.rate, WAV_SEGMENT_LIMIT) {
            Ok(file) => {
                self.discard_ring();
                tracing::info!(node = self.node.0, path = %path.display(), ?settings, "recording started");
                self.file = Some(OpenFile {
                    file,
                    base: path.to_path_buf(),
                    last_status: Instant::now(),
                });
                self.active.store(true, Ordering::Release);
                self.report_recording();
            }
            Err(error) => {
                tracing::warn!(node = self.node.0, path = %path.display(), %error, "cannot create the recording file");
                self.report(RecorderState::Failed {
                    reason: failure_reason(&error),
                });
            }
        }
    }

    /// 停止录音：先让调度线程停止推送，写完缓冲里剩下的数据，再结束文件。
    fn stop(&mut self) {
        self.active.store(false, Ordering::Release);
        self.drain();
        let Some(open) = self.file.take() else {
            return;
        };
        let frames = open.file.frames();
        match open.file.finish() {
            Ok(()) => {
                tracing::info!(node = self.node.0, frames, "recording finished");
                self.report(RecorderState::Idle);
            }
            Err(error) => {
                tracing::warn!(node = self.node.0, %error, "cannot finish the recording file");
                self.report(RecorderState::Failed {
                    reason: failure_reason(&error),
                });
            }
        }
    }

    fn discard_ring(&mut self) {
        let available = self.consumer.slots();
        if let Ok(chunk) = self.consumer.read_chunk(available) {
            chunk.commit_all();
        }
    }

    /// 把环形缓冲里的数据全部写进文件。出错时只停这一路。
    fn drain(&mut self) {
        let available = self.consumer.slots() / STEREO * STEREO;
        let Ok(chunk) = self.consumer.read_chunk(available) else {
            return;
        };
        let (first, second) = chunk.as_slices();
        let result = match self.file.as_mut() {
            Some(open) => open.file.write(first).and_then(|()| open.file.write(second)),
            None => Ok(()),
        };
        chunk.commit_all();
        if let Err(error) = result {
            self.fail(&error);
        }
    }

    /// 写盘出错：停止这一路，尽量把已写的部分收好尾，再报告原因。
    fn fail(&mut self, error: &io::Error) {
        tracing::warn!(node = self.node.0, %error, "recording failed");
        self.active.store(false, Ordering::Release);
        if let Some(open) = self.file.take() {
            let _ = open.file.finish();
        }
        self.report(RecorderState::Failed {
            reason: failure_reason(error),
        });
    }

    /// 每隔一段时间刷新文件头并向界面报告进度，这样崩溃时文件也是可读的。
    fn report_progress(&mut self) {
        let due = self
            .file
            .as_ref()
            .is_some_and(|open| open.last_status.elapsed() >= STATUS_INTERVAL);
        if !due {
            return;
        }
        let synced = match self.file.as_mut() {
            Some(open) => {
                open.last_status = Instant::now();
                open.file.sync()
            }
            None => Ok(()),
        };
        match synced {
            Ok(()) => self.report_recording(),
            Err(error) => self.fail(&error),
        }
    }

    fn report_recording(&self) {
        if let Some(open) = &self.file {
            self.report(RecorderState::Recording {
                file: open.file.current_path(&open.base).display().to_string(),
                frames: open.file.frames(),
                bytes: open.file.bytes(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_disk_is_told_apart_from_other_write_errors() {
        assert_eq!(
            failure_reason(&io::Error::from_raw_os_error(112)),
            RecordingFailure::DiskFull
        );
        assert_eq!(
            failure_reason(&io::Error::from(io::ErrorKind::PermissionDenied)),
            RecordingFailure::WriteFailed
        );
    }
}
