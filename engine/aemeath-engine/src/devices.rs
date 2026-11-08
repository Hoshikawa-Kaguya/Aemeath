//! 控制线程一侧对外部设备的管理：每个"端点 + 方向"一个 IO 线程（`aemeath-io`），主时钟跟随线程，
//! 以及端点变化的监听（变化时立即重新检测离线设备，选型报告 6.5 节）。
//!
//! 换拓扑时：
//! - 配置（引擎采样率、重采样算法、独占设置）没变、继续使用的设备，IO 线程不重启；它在引擎一侧的端口
//!   由新混音器从旧混音器接过（[`Mixer::inherit`]）。
//! - 配置变了的设备，先停掉旧 IO 线程（独占时必须先释放设备），再新建；调用方在此之前已让这些节点淡出
//!   （[`crate::Engine::apply_topology`]）。
//! - 新出现的设备，新建环形缓冲和 IO 线程，端口直接接到新混音器上。
//! - 不再使用的设备，停止它的 IO 线程（旧混音器里剩下的端口随旧混音器释放）。
//!
//! 每个"端点 + 方向"的时钟快照（[`ClockTap`]）在 IO 线程重建后保留，主时钟跟随线程不必重启。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use aemeath_io::capabilities::{CapabilityCache, DeviceProber};
use aemeath_io::clock::ClockTap;
use aemeath_io::device::IoContext;
use aemeath_io::follow::{ClockTarget, Follower};
use aemeath_io::redetect::Redetect;
use aemeath_io::{
    BridgeConfig, BridgeHandle, IoDirection, STEREO, StatusSink, input, output, stereo_ring, target_frames,
};
use aemeath_protocol::ids::{EndpointId, NodeId};
use aemeath_protocol::topology::{NodeKind, NodeRole, RouteTopology};
use aemeath_sys::clock::now_hns;

use crate::mixer::Mixer;
use crate::port::Port;
use crate::timeline::{Timeline, TimelineWriter};

type DeviceKey = (EndpointId, IoDirection);

/// 端点监听线程检查停止信号的间隔
const WATCH_POLL: Duration = Duration::from_millis(200);
/// 外部设备端口每次开始收发时的淡入时长（与通道条的增益平滑相同）
const PORT_FADE_SECONDS: f64 = 0.010;

/// 引擎的设备时间线，作为主时钟跟随的调整对象。
pub struct TimelineClock {
    pub timeline: Arc<Timeline>,
    pub writer: TimelineWriter,
}

impl ClockTarget for TimelineClock {
    fn position(&self, hns: i64) -> f64 {
        self.timeline.position(hns)
    }

    fn set_rate(&mut self, factor: f64, now_hns: i64) {
        self.writer.set_rate(factor, now_hns);
    }
}

enum Clock {
    /// 没有主时钟：时间线按 QPC 速率运行
    Free(Box<dyn ClockTarget>),
    Following {
        key: DeviceKey,
        follower: Follower,
    },
    /// 跟随线程异常退出，时间线的写入句柄已丢失（只记录，不再跟随）
    Lost,
}

struct Bridge {
    /// 只为持有：丢弃时停止 IO 线程并等它释放设备
    _thread: BridgeHandle,
    config: BridgeConfig,
}

pub struct ExternalDevices {
    bridges: HashMap<DeviceKey, Bridge>,
    taps: HashMap<DeviceKey, ClockTap>,
    clock: Clock,
    context: IoContext,
    watcher: Option<EndpointWatchThread>,
}

impl ExternalDevices {
    pub fn new(clock: TimelineClock, status: StatusSink) -> Self {
        let context = IoContext {
            status,
            capabilities: CapabilityCache::default(),
            redetect: Redetect::default(),
        };
        let watcher = EndpointWatchThread::spawn(context.redetect.clone())
            .inspect_err(|error| tracing::warn!(%error, "cannot watch audio endpoints, offline devices are only retried on a timer"))
            .ok();
        Self {
            bridges: HashMap::new(),
            taps: HashMap::new(),
            clock: Clock::Free(Box::new(clock)),
            context,
            watcher,
        }
    }

    /// 回答设备能力查询（主程序的"添加节点"和节点设置用，ADR-0014）。
    pub fn prober(&self) -> DeviceProber {
        DeviceProber::new(self.context.capabilities.clone())
    }

    /// 立即重新检测离线设备：`endpoints` 为 None 时全部。
    pub fn redetect(&self, endpoints: Option<&[EndpointId]>) {
        match endpoints {
            Some(endpoints) => self.context.redetect.wake_endpoints(endpoints.to_vec()),
            None => self.context.redetect.wake_all(),
        }
    }

    /// 外部设备节点在这份拓扑里要求的 IO 线程配置：(节点下标, 节点 ID, 设备, 配置)。
    /// 混音器的节点顺序与拓扑相同，所以节点下标也是混音器里的下标。
    fn wanted(topology: &RouteTopology) -> Vec<(usize, NodeId, DeviceKey, BridgeConfig)> {
        topology
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| {
                let endpoint = node.kind.device()?.clone();
                let settings = node.io.unwrap_or_default();
                let config = BridgeConfig {
                    endpoint: endpoint.clone(),
                    engine_rate: topology.engine_sample_rate,
                    quality: settings.resampler_or(topology.resampler),
                    access: settings.access,
                };
                Some((index, node.id, (endpoint, direction(node.kind.role())), config))
            })
            .collect()
    }

    /// 设备流要重建的节点：IO 线程已经在运行、但配置变了（换拓扑前要先让它们淡出）。
    pub fn reconfigured_nodes(&self, topology: &RouteTopology) -> Vec<NodeId> {
        Self::wanted(topology)
            .into_iter()
            .filter(|(_, _, key, config)| self.bridges.get(key).is_some_and(|bridge| bridge.config != *config))
            .map(|(_, node, _, _)| node)
            .collect()
    }

    /// 为新混音器准备外部设备：复用、新建或停止 IO 线程，并把新建的端口接到混音器上。
    pub fn prepare(&mut self, topology: &RouteTopology, mixer: &mut Mixer) {
        let wanted = Self::wanted(topology);
        let mut keep = HashSet::new();
        for (node, _, key, config) in wanted {
            keep.insert(key.clone());
            let unchanged = self.bridges.get(&key).is_some_and(|bridge| bridge.config == config);
            if unchanged && mixer.inherits_port(node) {
                continue;
            }
            // 先停旧线程：独占时它必须先释放设备，新线程才打得开
            if self.bridges.remove(&key).is_some() {
                tracing::info!(endpoint = %key.0.0, direction = ?key.1, "reopening the device with new settings");
            }
            let tap = self.taps.entry(key.clone()).or_default().clone();
            let (producer, consumer) = stereo_ring(topology.engine_sample_rate);
            let fade = (f64::from(topology.engine_sample_rate.hz()) * PORT_FADE_SECONDS).round() as u32;
            let spawned = match key.1 {
                IoDirection::Input => {
                    let prime = target_frames(topology.engine_sample_rate) * STEREO;
                    mixer.attach_port(node, Port::input(consumer, prime, fade));
                    input::spawn_input(config.clone(), producer, self.context.clone(), tap)
                }
                IoDirection::Output => {
                    let active = Arc::new(AtomicBool::new(false));
                    mixer.attach_port(node, Port::output(producer, Arc::clone(&active), fade));
                    output::spawn_output(config.clone(), consumer, active, self.context.clone(), tap)
                }
            };
            match spawned {
                Ok(handle) => {
                    self.bridges.insert(
                        key,
                        Bridge {
                            _thread: handle,
                            config,
                        },
                    );
                }
                Err(error) => tracing::error!(endpoint = %key.0.0, %error, "cannot start the device thread"),
            }
        }
        self.bridges.retain(|key, _| keep.contains(key));
        self.taps.retain(|key, _| keep.contains(key));
        self.update_clock(topology);
    }

    /// 按拓扑的主时钟切换跟随线程。
    fn update_clock(&mut self, topology: &RouteTopology) {
        let wanted = topology.clock_master.and_then(|master| {
            topology
                .nodes
                .iter()
                .find(|node| node.id == master)
                .and_then(|node| match &node.kind {
                    NodeKind::ExternalInput { device } => Some((device.clone(), IoDirection::Input)),
                    NodeKind::ExternalOutput { device } => Some((device.clone(), IoDirection::Output)),
                    _ => None,
                })
        });
        if let Clock::Following { key, .. } = &self.clock
            && Some(key) == wanted.as_ref()
        {
            return;
        }

        let target = match std::mem::replace(&mut self.clock, Clock::Lost) {
            Clock::Free(target) => Some(target),
            Clock::Following { follower, .. } => follower.stop(),
            Clock::Lost => None,
        };
        let Some(mut target) = target else {
            tracing::error!("the clock follower thread failed; the timeline can no longer follow a clock master");
            return;
        };
        let wanted = wanted.and_then(|key| self.taps.get(&key).cloned().map(|tap| (key, tap)));
        self.clock = match wanted {
            Some((key, tap)) => {
                tracing::info!(endpoint = %key.0.0, direction = ?key.1, "following clock master");
                match Follower::spawn(tap, target) {
                    Ok(follower) => Clock::Following { key, follower },
                    Err(error) => {
                        tracing::error!(%error, "cannot start the clock follower thread");
                        Clock::Lost
                    }
                }
            }
            None => {
                // 回到 QPC 速率；在当前时刻保持连续
                target.set_rate(1.0, now_hns());
                Clock::Free(target)
            }
        };
    }

    /// 停止所有 IO 线程、跟随线程和端点监听线程。
    pub fn shutdown(&mut self) {
        self.bridges.clear();
        if let Clock::Following { follower, .. } = std::mem::replace(&mut self.clock, Clock::Lost)
            && let Some(target) = follower.stop()
        {
            self.clock = Clock::Free(target);
        }
        self.watcher = None;
    }
}

fn direction(role: NodeRole) -> IoDirection {
    match role {
        NodeRole::Input => IoDirection::Input,
        _ => IoDirection::Output,
    }
}

/// 端点变化监听（`IMMNotificationClient`）：任何端点增减或状态变化，立即唤醒所有离线设备的重试。
/// COM 对象只能在创建它的线程上使用，所以由一个专用线程持有；丢弃时线程注销通知并退出。
struct EndpointWatchThread {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl EndpointWatchThread {
    fn spawn(redetect: Redetect) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("aemeath-endpoint-watch".to_owned())
            .spawn(move || {
                // 局部变量按声明的逆序释放：先注销通知、释放枚举器，最后才离开 COM 套间。
                // （放进一个元组会按字段顺序释放，先离开套间再释放 COM 对象，会访问冲突。）
                let _apartment = match aemeath_sys::com::ComApartment::multithreaded() {
                    Ok(apartment) => apartment,
                    Err(error) => {
                        tracing::warn!(%error, "cannot initialize COM for the endpoint watcher");
                        return;
                    }
                };
                let endpoints = match aemeath_sys::audio_endpoints::AudioEndpoints::new() {
                    Ok(endpoints) => endpoints,
                    Err(error) => {
                        tracing::warn!(%error, "cannot enumerate audio endpoints");
                        return;
                    }
                };
                let _watcher = match endpoints.watch(move |_| redetect.wake_all()) {
                    Ok(watcher) => watcher,
                    Err(error) => {
                        tracing::warn!(%error, "cannot register for audio endpoint notifications");
                        return;
                    }
                };
                while !thread_stop.load(Ordering::Relaxed) {
                    thread::sleep(WATCH_POLL);
                }
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for EndpointWatchThread {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
