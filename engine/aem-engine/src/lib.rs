//! 主时钟调度（S4）与路由计划的执行。
//!
//! - [`timeline`]：速率可调的设备时间线（无锁）。
//! - [`mixer`]：路由计划的执行（通道条、连线、求和、平滑、电平）。
//! - [`slot`]：被导入槽位的流状态。
//! - [`scheduler`]：唯一的实时线程。
//! - [`Engine`]：控制线程一侧的接口：启动、下发计划、取事件与电平。
//!
//! 外部设备（WASAPI）、重采样桥和主时钟跟随在 M1 第 5 项加入；在那之前外部输入为静音，外部输出被丢弃，
//! 时间线按 QPC 速率运行。

pub mod fifo;
pub mod meters;
pub mod mixer;
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

use aem_protocol::ids::SlotId;
use aem_protocol::message::DeviceFormat;
use aem_protocol::plan::{NodeKind, PlanError, RoutePlan};
use aem_sys::clock::now_hns;
use aem_usbip::identity::IdentityProfile;
use aem_usbip::server::{ServerConfig, UsbipServer};

use crate::meters::MeterBank;
use crate::mixer::{Mixer, MixerLayout};
use crate::scheduler::{Command, Scheduler};
use crate::timeline::{Timeline, TimelineWriter};

/// 引擎内部一律是立体声（N 声道节点见选型报告第 10 节）
pub const CHANNELS: usize = 2;

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
}

pub struct Engine {
    server: UsbipServer,
    commands: Sender<Command>,
    retired: Receiver<Box<Mixer>>,
    events: Receiver<EngineEvent>,
    layout: Option<MixerLayout>,
    meters: Option<Arc<MeterBank>>,
    /// 主时钟跟随（第 5 项）使用
    _timeline_writer: TimelineWriter,
    scheduler: Option<JoinHandle<()>>,
}

impl Engine {
    /// 启动 USB/IP 服务端和调度线程。此时没有计划，所有槽位都不导出。
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
        let scheduler = Scheduler::new(
            timeline,
            command_receiver,
            server_receiver,
            retired_sender,
            event_sender,
        );
        let handle = thread::Builder::new()
            .name("aem-scheduler".to_owned())
            .spawn(move || scheduler.run())?;
        Ok(Self {
            server,
            commands,
            retired,
            events,
            layout: None,
            meters: None,
            _timeline_writer: timeline_writer,
            scheduler: Some(handle),
        })
    }

    pub fn usbip_address(&self) -> SocketAddr {
        self.server.local_addr()
    }

    /// 校验并下发一份路由计划：导出它占用的槽位，构建混音器交给调度线程。
    pub fn apply_plan(&mut self, plan: &RoutePlan) -> Result<(), PlanError> {
        plan.validate()?;
        // 先释放调度线程送回的旧混音器
        while self.retired.try_recv().is_ok() {}

        let meters = Arc::new(MeterBank::new(plan.nodes.iter().map(|node| node.id).collect()));
        let (mixer, layout) = Mixer::build(plan, self.layout.as_ref(), Arc::clone(&meters));
        let slots: BTreeSet<SlotId> = plan
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

    /// 等待下一个事件，最多 `timeout`。
    pub fn next_event(&self, timeout: Duration) -> Option<EngineEvent> {
        self.events.recv_timeout(timeout).ok()
    }

    /// 当前计划的电平（节点顺序与计划一致）。
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
    }
}
