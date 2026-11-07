//! 控制线程一侧对外部设备的管理：每个"端点 + 方向"一个 IO 线程（`aemeath-io`），以及主时钟跟随线程。
//!
//! 换拓扑时：
//! - 继续使用的设备，IO 线程不重启；它在引擎一侧的端口由新混音器从旧混音器接过（[`Mixer::inherit`]）。
//! - 新出现的设备，新建环形缓冲和 IO 线程，端口直接接到新混音器上。
//! - 不再使用的设备，停止它的 IO 线程（旧混音器里剩下的端口随旧混音器释放）。
//! - 引擎采样率或重采样算法变了：比率在构建时确定，所有 IO 线程重建。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use aemeath_io::follow::{ClockTarget, Follower};
use aemeath_io::{
    BridgeConfig, BridgeHandle, IoDirection, STEREO, StatusSink, input, output, stereo_ring, target_frames,
};
use aemeath_protocol::ids::EndpointId;
use aemeath_protocol::topology::{NodeKind, NodeRole, Resampler, RouteTopology};
use aemeath_protocol::units::SampleRate;
use aemeath_sys::clock::now_hns;

use crate::mixer::Mixer;
use crate::port::Port;
use crate::timeline::{Timeline, TimelineWriter};

type DeviceKey = (EndpointId, IoDirection);

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

pub struct ExternalDevices {
    bridges: HashMap<DeviceKey, BridgeHandle>,
    settings: Option<(SampleRate, Resampler)>,
    clock: Clock,
    status: StatusSink,
}

impl ExternalDevices {
    pub fn new(clock: TimelineClock, status: StatusSink) -> Self {
        Self {
            bridges: HashMap::new(),
            settings: None,
            clock: Clock::Free(Box::new(clock)),
            status,
        }
    }

    /// 为新混音器准备外部设备：复用、新建或停止 IO 线程，并把新建的端口接到混音器上。
    pub fn prepare(&mut self, topology: &RouteTopology, mixer: &mut Mixer) {
        let settings = (topology.engine_sample_rate, topology.resampler);
        if self.settings != Some(settings) {
            if !self.bridges.is_empty() {
                tracing::info!("engine sample rate or resampler changed, reopening external devices");
            }
            self.bridges.clear();
            self.settings = Some(settings);
        }

        let mut wanted = HashSet::new();
        for (node, endpoint, role) in mixer.external_nodes() {
            let direction = direction(role);
            let key = (endpoint.clone(), direction);
            wanted.insert(key.clone());
            if self.bridges.contains_key(&key) && mixer.inherits_port(node) {
                continue;
            }
            self.bridges.remove(&key);
            let config = BridgeConfig {
                endpoint,
                engine_rate: topology.engine_sample_rate,
                quality: topology.resampler,
            };
            let (producer, consumer) = stereo_ring(topology.engine_sample_rate);
            let spawned = match direction {
                IoDirection::Input => {
                    let prime = target_frames(topology.engine_sample_rate) * STEREO;
                    mixer.attach_port(node, Port::input(consumer, prime));
                    input::spawn_input(config, producer, Arc::clone(&self.status))
                }
                IoDirection::Output => {
                    let active = Arc::new(AtomicBool::new(false));
                    mixer.attach_port(node, Port::output(producer, Arc::clone(&active)));
                    output::spawn_output(config, consumer, active, Arc::clone(&self.status))
                }
            };
            match spawned {
                Ok(handle) => {
                    self.bridges.insert(key, handle);
                }
                Err(error) => tracing::error!(endpoint = %key.0.0, %error, "cannot start the device thread"),
            }
        }
        self.bridges.retain(|key, _| wanted.contains(key));
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
                    NodeKind::ExternalInput { endpoint } => Some((endpoint.clone(), IoDirection::Input)),
                    NodeKind::ExternalOutput { endpoint } => Some((endpoint.clone(), IoDirection::Output)),
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
        self.clock = match wanted {
            Some((endpoint, direction)) => {
                tracing::info!(endpoint = %endpoint.0, ?direction, "following clock master");
                match Follower::spawn(endpoint.clone(), direction, target) {
                    Ok(follower) => Clock::Following {
                        key: (endpoint, direction),
                        follower,
                    },
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

    /// 停止所有 IO 线程和跟随线程。
    pub fn shutdown(&mut self) {
        self.bridges.clear();
        if let Clock::Following { follower, .. } = std::mem::replace(&mut self.clock, Clock::Lost)
            && let Some(target) = follower.stop()
        {
            self.clock = Clock::Free(target);
        }
    }
}

fn direction(role: NodeRole) -> IoDirection {
    match role {
        NodeRole::Input => IoDirection::Input,
        _ => IoDirection::Output,
    }
}
