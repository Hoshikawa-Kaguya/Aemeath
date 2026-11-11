//! 槽位对应的音频端点：查询状态与设备格式、设定格式、监听变化。
//!
//! 线程模型：COM 对象只能在创建它的线程上使用，所以由一个专用线程持有（MTA），其他代码经请求通道访问。
//! 端点变化通知在系统线程上回调，只转发一个"有变化"的信号，具体状态由下一轮检查重新查询。
//!
//! 槽位与端点的对应：设备接口名（USB 产品名，如 `Aemeath Dev In 1`）完全相同，且方向一致。
//! 虚拟输入（In）是播放端点，虚拟输出（Out）是录音端点。

use std::collections::BTreeMap;
use std::sync::mpsc as std_mpsc;

use aemeath_protocol::ids::{SlotDirection, SlotId};
use aemeath_protocol::message::{DeviceFormat, SampleFormat};
use aemeath_protocol::units::SampleRate;
use aemeath_sys::audio_endpoints::{AudioEndpoints, AudioFlow, EndpointInfo, EndpointState, EndpointWatcher};
use aemeath_sys::com::{ComApartment, ComError};
use aemeath_sys::policy_config::PolicyConfig;
use aemeath_usbip::identity::SlotIdentity;
use thiserror::Error;
use tokio::sync::oneshot;
use tracing::{debug, info, warn};

/// 一个槽位的端点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotEndpoint {
    pub id: String,
    /// Windows 显示的端点名，例如"扬声器 (Aemeath Dev In 1)"
    pub name: Option<String>,
    pub state: EndpointState,
    /// Windows 为端点确定的设备格式（不在线时也能读到）
    pub format: Option<DeviceFormat>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("audio endpoint query failed: {0}")]
pub struct ProbeError(String);

impl ProbeError {
    pub(crate) fn new(message: String) -> Self {
        Self(message)
    }
}

impl From<ComError> for ProbeError {
    fn from(error: ComError) -> Self {
        Self(error.to_string())
    }
}

/// 一台外部设备（不属于 Aemeath 的端点），供新建外部输入、外部输出节点时选择。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalDevice {
    pub endpoint_id: String,
    /// 端点名，例如"耳机 (LG ULTRAGEAR+)"
    pub name: String,
}

type Reply<T> = oneshot::Sender<Result<T, ProbeError>>;

enum Request {
    Snapshot(Reply<BTreeMap<SlotId, SlotEndpoint>>),
    SlotEndpoints(Reply<BTreeMap<SlotId, SlotEndpoint>>),
    ExternalDevices {
        flow: AudioFlow,
        reply: Reply<Vec<ExternalDevice>>,
    },
    /// 改设备格式：采样率改成 `rate`；`sample_format` 为 None 时样本格式不变，声道数总是不变
    SetFormat {
        endpoint_id: String,
        rate: SampleRate,
        sample_format: Option<SampleFormat>,
        reply: Reply<DeviceFormat>,
    },
}

/// 端点查询线程的句柄。丢弃时线程退出。
#[derive(Debug)]
pub struct EndpointProbe {
    requests: std_mpsc::Sender<Request>,
}

impl EndpointProbe {
    /// 启动查询线程。`on_change` 在端点有增减或状态变化时调用（在系统线程上，不能阻塞）。
    pub fn start(slots: Vec<SlotIdentity>, on_change: impl Fn() + Send + Sync + 'static) -> Result<Self, ProbeError> {
        let (requests, receiver) = std_mpsc::channel();
        let (ready, started) = std_mpsc::channel();
        std::thread::Builder::new()
            .name("aemeath-endpoints".to_owned())
            .spawn(move || worker(slots, on_change, receiver, ready))
            .map_err(|error| ProbeError(error.to_string()))?;
        started.recv().map_err(|error| ProbeError(error.to_string()))??;
        Ok(Self { requests })
    }

    /// 16 个槽位各自的端点（找不到的槽位不在结果中）。
    pub async fn snapshot(&self) -> Result<BTreeMap<SlotId, SlotEndpoint>, ProbeError> {
        let (reply, wait) = oneshot::channel();
        self.send(Request::Snapshot(reply))?;
        wait.await.map_err(|_| stopped())?
    }

    /// 同 [`Self::snapshot`]，但不读设备格式（`format` 都是 None）。
    /// 只需要端点 ID 和名字时用它：没读过格式的离线槽位，每个要等 90–170 ms 才失败（实测），全部读一遍超过 1 s。
    pub async fn slot_endpoints(&self) -> Result<BTreeMap<SlotId, SlotEndpoint>, ProbeError> {
        let (reply, wait) = oneshot::channel();
        self.send(Request::SlotEndpoints(reply))?;
        wait.await.map_err(|_| stopped())?
    }

    /// 把端点的设备格式改成指定的采样率和样本格式（`sample_format` 为 None 时不变），返回设定后的格式。
    /// 端点上已有的流会被作废，正在使用这个设备的应用会短暂断开。
    pub async fn set_format(
        &self,
        endpoint_id: String,
        rate: SampleRate,
        sample_format: Option<SampleFormat>,
    ) -> Result<DeviceFormat, ProbeError> {
        let (reply, wait) = oneshot::channel();
        self.send(Request::SetFormat {
            endpoint_id,
            rate,
            sample_format,
            reply,
        })?;
        wait.await.map_err(|_| stopped())?
    }

    /// 某个方向上全部在线的外部设备（不含 Aemeath 的槽位），按名称排序。
    pub async fn external_devices(&self, flow: AudioFlow) -> Result<Vec<ExternalDevice>, ProbeError> {
        let (reply, wait) = oneshot::channel();
        self.send(Request::ExternalDevices { flow, reply })?;
        wait.await.map_err(|_| stopped())?
    }

    fn send(&self, request: Request) -> Result<(), ProbeError> {
        self.requests.send(request).map_err(|_| stopped())
    }
}

fn stopped() -> ProbeError {
    ProbeError("the endpoint thread stopped".to_owned())
}

struct Worker {
    slots: Vec<SlotIdentity>,
    endpoints: AudioEndpoints,
    policy: PolicyConfig,
    _watcher: EndpointWatcher,
}

/// COM 线程的主体：建好 COM 对象并订阅端点变化后报告就绪，然后逐个执行请求，直到请求通道关闭。
fn worker(
    slots: Vec<SlotIdentity>,
    on_change: impl Fn() + Send + Sync + 'static,
    requests: std_mpsc::Receiver<Request>,
    ready: std_mpsc::Sender<Result<(), ProbeError>>,
) {
    let setup = || -> Result<(ComApartment, Worker), ProbeError> {
        let com = ComApartment::multithreaded()?;
        let endpoints = AudioEndpoints::new()?;
        let policy = PolicyConfig::new()?;
        let watcher = endpoints.watch(move |change| {
            debug!(?change, "audio endpoint changed");
            on_change();
        })?;
        Ok((
            com,
            Worker {
                slots,
                endpoints,
                policy,
                _watcher: watcher,
            },
        ))
    };
    // 注意：Worker 必须先于 ComApartment 释放（COM 对象要在退出套间之前释放）
    let (_com, worker) = match setup() {
        Ok(parts) => parts,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    while let Ok(first) = requests.recv() {
        // 系统阻塞查询期间排队的请求一并处理：多个查询只做一次
        let mut batch = vec![first];
        batch.extend(requests.try_iter());
        let mut snapshot_replies = Vec::new();
        for request in batch {
            match request {
                Request::Snapshot(reply) => snapshot_replies.push(reply),
                Request::SlotEndpoints(reply) => {
                    let _ = reply.send(worker.slot_endpoints());
                }
                Request::ExternalDevices { flow, reply } => {
                    let _ = reply.send(worker.external_devices(flow));
                }
                Request::SetFormat {
                    endpoint_id,
                    rate,
                    sample_format,
                    reply,
                } => {
                    let _ = reply.send(worker.set_format(&endpoint_id, rate, sample_format));
                }
            }
        }
        if !snapshot_replies.is_empty() {
            let snapshot = worker.snapshot();
            for reply in snapshot_replies {
                let _ = reply.send(snapshot.clone());
            }
        }
    }
    drop(worker);
}

impl Worker {
    fn snapshot(&self) -> Result<BTreeMap<SlotId, SlotEndpoint>, ProbeError> {
        let mut result = self.slot_endpoints()?;
        for endpoint in result.values_mut() {
            endpoint.format = self.policy.device_format(&endpoint.id).ok();
        }
        Ok(result)
    }

    fn slot_endpoints(&self) -> Result<BTreeMap<SlotId, SlotEndpoint>, ProbeError> {
        let render = self.endpoints.list(AudioFlow::Render)?;
        let capture = self.endpoints.list(AudioFlow::Capture)?;
        let mut result = BTreeMap::new();
        for identity in &self.slots {
            let candidates = match identity.slot.direction {
                SlotDirection::In => &render,
                SlotDirection::Out => &capture,
            };
            if let Some(info) = pick(candidates, &identity.product_name) {
                result.insert(
                    identity.slot,
                    SlotEndpoint {
                        id: info.id.clone(),
                        name: info.name.clone(),
                        state: info.state,
                        format: None,
                    },
                );
            }
        }
        Ok(result)
    }

    fn external_devices(&self, flow: AudioFlow) -> Result<Vec<ExternalDevice>, ProbeError> {
        let mut devices: Vec<ExternalDevice> = self
            .endpoints
            .list(flow)?
            .into_iter()
            .filter(|info| info.state == EndpointState::Active)
            .filter(|info| !self.is_slot(info))
            .map(|info| ExternalDevice {
                name: info.name.unwrap_or_else(|| info.id.clone()),
                endpoint_id: info.id,
            })
            .collect();
        devices.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(devices)
    }

    /// 端点是否属于 Aemeath 的某个槽位。把槽位当作外部设备接进来会形成回路，所以不列出。
    fn is_slot(&self, info: &EndpointInfo) -> bool {
        info.interface_name
            .as_deref()
            .is_some_and(|name| self.slots.iter().any(|identity| identity.product_name == name))
    }

    /// 把端点的共享格式设成指定采样率；不指定样本格式时保留当前的。返回设定后的格式。
    fn set_format(
        &self,
        endpoint_id: &str,
        rate: SampleRate,
        sample_format: Option<SampleFormat>,
    ) -> Result<DeviceFormat, ProbeError> {
        let current = self
            .policy
            .device_format(endpoint_id)
            .map_err(|error| ProbeError(error.to_string()))?;
        let wanted = DeviceFormat {
            sample_rate: rate,
            sample_format: sample_format.unwrap_or(current.sample_format),
            ..current
        };
        self.policy
            .set_device_format(endpoint_id, wanted)
            .map_err(|error| ProbeError(error.to_string()))?;
        info!(
            endpoint_id,
            from = current.sample_rate.hz(),
            to = rate.hz(),
            sample_format = ?wanted.sample_format,
            "slot format set"
        );
        Ok(wanted)
    }
}

/// 接口名完全相同的端点；有多个时（例如系统里留有旧实例）优先 ACTIVE 的。
fn pick<'a>(candidates: &'a [EndpointInfo], product_name: &str) -> Option<&'a EndpointInfo> {
    let mut matching = candidates
        .iter()
        .filter(|info| info.interface_name.as_deref() == Some(product_name));
    let first = matching.next()?;
    if first.state == EndpointState::Active {
        return Some(first);
    }
    let active = matching.find(|info| info.state == EndpointState::Active);
    if active.is_some() {
        warn!(product_name, "more than one endpoint for a slot");
    }
    Some(active.unwrap_or(first))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: &str, name: &str, state: EndpointState) -> EndpointInfo {
        EndpointInfo {
            id: id.to_owned(),
            flow: AudioFlow::Render,
            state,
            name: None,
            interface_name: Some(name.to_owned()),
        }
    }

    #[test]
    fn picks_the_exact_interface_name_and_prefers_active_endpoints() {
        let candidates = [
            info("a", "Aemeath Dev In 10", EndpointState::Active),
            info("b", "Aemeath Dev In 1", EndpointState::NotPresent),
            info("c", "Aemeath Dev In 1", EndpointState::Active),
        ];
        assert_eq!(
            pick(&candidates, "Aemeath Dev In 1").map(|info| info.id.as_str()),
            Some("c")
        );
        assert_eq!(
            pick(&candidates[..2], "Aemeath Dev In 1").map(|info| info.id.as_str()),
            Some("b")
        );
        assert!(pick(&candidates, "Aemeath Dev In 2").is_none());
    }
}
