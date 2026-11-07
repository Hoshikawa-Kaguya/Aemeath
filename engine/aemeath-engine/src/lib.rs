//! 主时钟调度（S4）与路由拓扑的执行。
//!
//! - [`timeline`]：速率可调的设备时间线（无锁）。
//! - [`mixer`]：路由拓扑的执行（通道条、连线、求和、平滑、电平）。
//! - [`slot`]：被导入槽位的流状态。
//! - [`scheduler`]：唯一的实时线程。
//! - [`port`]：外部设备节点在引擎一侧的端口。
//! - [`devices`]：外部设备 IO 线程与主时钟跟随的管理。
//! - [`Engine`]：控制线程一侧的接口：启动、下发拓扑、取事件与电平。

pub mod devices;
pub mod fifo;
pub mod meters;
pub mod mixer;
pub mod port;
pub mod scheduler;
pub mod slot;
pub mod timeline;

use std::collections::BTreeSet;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use aemeath_io::IoEvent;
use aemeath_protocol::ids::SlotId;
use aemeath_protocol::message::DeviceFormat;
use aemeath_protocol::topology::{NodeKind, RouteTopology, TopologyError};
use aemeath_sys::clock::now_hns;
use aemeath_usbip::identity::IdentityProfile;
use aemeath_usbip::server::{ServerConfig, UsbipServer};

use crate::devices::{ExternalDevices, TimelineClock};
use crate::meters::MeterBank;
use crate::mixer::{Mixer, MixerLayout};
use crate::scheduler::{Command, Scheduler};
use crate::timeline::Timeline;

/// 引擎内部一律是立体声（N 声道节点见选型报告第 10 节）
pub use aemeath_dsp::STEREO;

#[derive(Debug, Clone, Copy)]
pub struct EngineConfig {
    pub usbip_port: u16,
    pub identity: IdentityProfile,
}

/// 引擎报告给控制面的事件（再由宿主转成 IPC 消息）。
#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    /// USB/IP 服务端上某个槽位的连接状态（ADR-0007：挂载成功以收到 import 为准）
    SlotConnection { slot: SlotId, connected: bool },
    /// 主机为槽位选定的流格式；None 表示流停止
    SlotFormat { slot: SlotId, format: Option<DeviceFormat> },
    /// 外部设备的状态（在线及其格式，或离线及原因）
    Endpoint(IoEvent),
}

pub struct Engine {
    server: UsbipServer,
    commands: Sender<Command>,
    retired: Receiver<Box<Mixer>>,
    /// 被 [`Engine::take_events`] 取走后为 None
    events: Option<Receiver<EngineEvent>>,
    layout: Option<MixerLayout>,
    meters: Option<Arc<MeterBank>>,
    devices: ExternalDevices,
    scheduler: Option<JoinHandle<()>>,
}

impl Engine {
    /// 启动 USB/IP 服务端和调度线程。此时没有拓扑，所有槽位都不导出。
    pub fn start(config: EngineConfig) -> io::Result<Self> {
        let (server_events, server_receiver) = mpsc::channel();
        let server = UsbipServer::start(
            ServerConfig {
                port: config.usbip_port,
                profile: config.identity,
            },
            server_events,
        )?;
        let (commands, command_receiver) = mpsc::channel();
        let (retired_sender, retired) = mpsc::channel();
        let (event_sender, events) = mpsc::channel();
        let (timeline, timeline_writer) = Timeline::new(now_hns());
        let device_events = event_sender.clone();
        let devices = ExternalDevices::new(
            TimelineClock {
                timeline: Arc::clone(&timeline),
                writer: timeline_writer,
            },
            Arc::new(move |event| {
                let _ = device_events.send(EngineEvent::Endpoint(event));
            }),
        );
        let scheduler = Scheduler::new(
            timeline,
            command_receiver,
            server_receiver,
            retired_sender,
            event_sender,
        );
        let handle = thread::Builder::new()
            .name("aemeath-scheduler".to_owned())
            .spawn(move || scheduler.run())?;
        Ok(Self {
            server,
            commands,
            retired,
            events: Some(events),
            layout: None,
            meters: None,
            devices,
            scheduler: Some(handle),
        })
    }

    pub fn usbip_address(&self) -> SocketAddr {
        self.server.local_addr()
    }

    /// 校验并下发一份路由拓扑：导出它占用的槽位，构建混音器交给调度线程。
    pub fn apply_topology(&mut self, topology: &RouteTopology) -> Result<(), TopologyError> {
        topology.validate()?;
        // 先释放调度线程送回的旧混音器
        while self.retired.try_recv().is_ok() {}

        let meters = Arc::new(MeterBank::new(topology.nodes.iter().map(|node| node.id).collect()));
        let (mut mixer, layout) = Mixer::build(topology, self.layout.as_ref(), Arc::clone(&meters));
        self.devices.prepare(topology, &mut mixer);
        let slots: BTreeSet<SlotId> = topology
            .nodes
            .iter()
            .filter_map(|node| match node.kind {
                NodeKind::VirtualInput { slot } | NodeKind::VirtualOutput { slot } => Some(slot),
                _ => None,
            })
            .collect();
        self.server.set_exported(slots);
        if self.commands.send(Command::Apply(Box::new(mixer))).is_err() {
            tracing::error!("the scheduler thread has stopped");
        }
        self.layout = Some(layout);
        self.meters = Some(meters);
        Ok(())
    }

    /// 等待下一个事件，最多 `timeout`。事件接收端已被 [`Self::take_events`] 取走时返回 None。
    pub fn next_event(&self, timeout: Duration) -> Option<EngineEvent> {
        self.events.as_ref()?.recv_timeout(timeout).ok()
    }

    /// 取走事件接收端，交给专门的线程接收（宿主进程这样做，以便同时在另一处调用 [`Self::apply_topology`]）。
    pub fn take_events(&mut self) -> Option<Receiver<EngineEvent>> {
        self.events.take()
    }

    /// 当前拓扑的电平（节点顺序与拓扑一致）。
    pub fn meters(&self) -> Option<&Arc<MeterBank>> {
        self.meters.as_ref()
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(handle) = self.scheduler.take() {
            let _ = handle.join();
        }
        self.devices.shutdown();
    }
}
