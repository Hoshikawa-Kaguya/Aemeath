//! 外部设备的 WASAPI 共享流、异步采样率转换（ASRC）桥接，以及主时钟跟随。
//!
//! 每台外部设备一个 IO 线程（ADR-0008 的线程模型），与引擎的调度线程之间只通过无锁环形缓冲（rtrb）交换音频：
//! - 外部输入：设备 → 转立体声 → ASRC 到引擎采样率 → 环形缓冲 → 调度线程每 1 ms 取一块。
//! - 外部输出：调度线程每 1 ms 推一块 → 环形缓冲 → ASRC 到设备采样率 → 设备。
//!
//! ASRC 的比率由 IO 线程里的 PI 控制器按环形缓冲的水位调整（S5），调度线程不参与。
//! 环形缓冲在引擎一侧的那一端被丢弃时（拓扑不再使用这台设备），IO 线程自行关闭设备并退出。

pub mod bridge;
pub mod device;
pub mod follow;
pub mod input;
pub mod output;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use aemeath_protocol::ids::EndpointId;
use aemeath_protocol::message::EndpointStatus;
use aemeath_protocol::topology::Resampler;
use aemeath_protocol::units::SampleRate;
use rtrb::{Consumer, Producer, RingBuffer};

/// 环形缓冲的目标水位（S5 默认值）。外部输入在引擎一侧先攒够这么多才开始读取。
pub const TARGET_LEVEL_MS: f64 = 15.0;
/// 环形缓冲容量（ms），远大于目标水位
const RING_CAPACITY_MS: usize = 200;
const MILLISECONDS_PER_SECOND: usize = 1000;
/// 引擎内部一律是立体声
pub const STEREO: usize = 2;

/// 从 Aemeath 的视角命名：输入是录音端点，输出是播放端点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IoDirection {
    Input,
    Output,
}

/// 外部设备状态的变化。
#[derive(Debug, Clone, PartialEq)]
pub struct IoEvent {
    pub endpoint: EndpointId,
    pub direction: IoDirection,
    pub status: EndpointStatus,
}

/// IO 线程报告状态的回调（由引擎转成自己的事件）。
pub type StatusSink = Arc<dyn Fn(IoEvent) + Send + Sync>;

#[derive(Debug, Clone)]
pub struct BridgeConfig {
    pub endpoint: EndpointId,
    pub engine_rate: SampleRate,
    pub quality: Resampler,
}

/// 一个 IO 线程。析构时通知线程停止，并等它关闭设备后退出。
#[derive(Debug)]
pub struct BridgeHandle {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl BridgeHandle {
    fn new(stop: Arc<AtomicBool>, thread: JoinHandle<()>) -> Self {
        Self {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for BridgeHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// 一个立体声环形缓冲，容量 [`RING_CAPACITY_MS`]。
pub fn stereo_ring(engine_rate: SampleRate) -> (Producer<f32>, Consumer<f32>) {
    let frames = engine_rate.hz() as usize * RING_CAPACITY_MS / MILLISECONDS_PER_SECOND;
    RingBuffer::new(frames * STEREO)
}

/// 目标水位对应的帧数。
pub fn target_frames(rate: SampleRate) -> usize {
    (f64::from(rate.hz()) * TARGET_LEVEL_MS / MILLISECONDS_PER_SECOND as f64) as usize
}

/// 帧数换算成毫秒。
fn frames_to_ms(frames: usize, rate: SampleRate) -> f64 {
    frames as f64 * MILLISECONDS_PER_SECOND as f64 / f64::from(rate.hz())
}
