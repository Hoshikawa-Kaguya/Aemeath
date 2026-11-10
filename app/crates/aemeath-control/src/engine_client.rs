//! 与引擎的 IPC 客户端（协议见 `crates/aemeath-protocol/README.md`）。
//!
//! - 后台任务负责连接、握手、心跳和断线重连；调用方只提交"当前想要的路由拓扑"，并从事件通道读取结果和引擎状态。
//! - 拓扑以最新一份为准：断线期间提交多次时，重连后只下发最后一份。修订号由客户端分配，跨连接也只增不减。
//! - 心跳：每 [`Timing::ping_interval`] 发一次 `ping`，超过 [`Timing::unresponsive_after`] 没收到任何消息就判为假死，
//!   断开后重连。重启假死的引擎由守护进程负责（ADR-0008）。
//! - tokio 的管道客户端默认带 `SECURITY_IDENTIFICATION`：服务端不能借这条连接冒充主程序的身份。

use std::time::Duration;

use aemeath_protocol::PROTOCOL_VERSION;
use aemeath_protocol::frame::{self, FrameDecoder};
use aemeath_protocol::ids::{EndpointId, NodeId, ProtocolVersion, RequestId, SlotId, TopologyRevision};
use aemeath_protocol::message::{
    AppToEngine, DeviceCapabilities, DeviceFormat, EndpointDirection, EndpointStatus, EngineToApp, RecorderState,
    RejectReason,
};
use aemeath_protocol::topology::RouteTopology;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;
use tracing::{debug, info, warn};

/// 管道忙（服务端的实例都在用）
const ERROR_PIPE_BUSY: i32 = 231;
const READ_BUFFER: usize = 64 * 1024;
/// 请求引擎退出后，最多等这么久的应答
const SHUTDOWN_ACK_TIMEOUT: Duration = Duration::from_secs(2);

/// 时间参数。默认值用于产品；测试可以调短。
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// 连不上时的重试间隔
    pub reconnect_interval: Duration,
    /// 发出 `hello` 后等待 `welcome` 的时间（与引擎的握手时限相同）
    pub welcome_timeout: Duration,
    pub ping_interval: Duration,
    /// 超过这段时间没有收到任何消息，判为引擎假死
    pub unresponsive_after: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            reconnect_interval: Duration::from_millis(500),
            welcome_timeout: Duration::from_secs(5),
            ping_interval: Duration::from_secs(1),
            unresponsive_after: Duration::from_secs(3),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub pipe_name: String,
    /// 主程序版本，只用于引擎的日志
    pub app_version: String,
    pub timing: Timing,
}

/// 连接断开的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisconnectReason {
    /// 管道被关闭或读写出错（引擎退出、崩溃）
    Closed,
    /// 心跳超时
    Unresponsive,
    /// 没有在时限内完成握手
    HandshakeTimeout,
    /// 协议版本与引擎不同（引擎和主程序版本不配套）
    VersionMismatch { engine_protocol: ProtocolVersion },
    /// 收到无法解析的消息
    ProtocolError(String),
}

/// 客户端事件。
#[derive(Debug, Clone, PartialEq)]
pub enum ClientEvent {
    /// 握手完成。`restored_topology` 是引擎启动时从缓存恢复的拓扑修订号（ADR-0008）
    Connected {
        restored_topology: Option<TopologyRevision>,
        /// USB/IP 服务端实际监听的端口
        usbip_port: u16,
    },
    Disconnected(DisconnectReason),
    TopologyApplied {
        revision: TopologyRevision,
    },
    /// 拓扑被拒绝：主程序编译出的拓扑不该出现这种情况，`detail` 只写日志
    TopologyRejected {
        revision: TopologyRevision,
        reason: RejectReason,
        detail: String,
    },
    EndpointStatus {
        node: NodeId,
        status: EndpointStatus,
    },
    RecorderStatus {
        node: NodeId,
        status: RecorderState,
    },
    /// USB/IP 服务端上槽位的连接状态（挂载成功以此为准，ADR-0007）
    SlotConnection {
        slot: SlotId,
        connected: bool,
    },
    SlotFormat {
        slot: SlotId,
        format: Option<DeviceFormat>,
    },
}

enum Command {
    /// 请引擎退出；应答或断开后通知 `done`
    ShutdownEngine {
        done: oneshot::Sender<bool>,
    },
    /// 查询外部设备的能力；断开或探测失败时答复 None
    ProbeDevice {
        endpoint: EndpointId,
        direction: EndpointDirection,
        reply: oneshot::Sender<Option<DeviceCapabilities>>,
    },
    /// 立即重新检测离线设备；没有连接时直接丢弃
    Redetect {
        node: Option<NodeId>,
    },
    StartRecording {
        node: NodeId,
        path: String,
    },
    StopRecording {
        node: NodeId,
    },
}

type ProbeWaiters = Vec<(RequestId, oneshot::Sender<Option<DeviceCapabilities>>)>;

/// 设备探测的最长等待（引擎在阻塞线程上探测，通常约 100 ms）
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// 客户端句柄。丢弃时后台任务随之结束（不会让引擎退出）。
#[derive(Debug)]
pub struct EngineClient {
    desired: watch::Sender<Option<RouteTopology>>,
    commands: mpsc::UnboundedSender<Command>,
    task: tokio::task::JoinHandle<()>,
}

impl EngineClient {
    /// 在当前 tokio 运行时上启动后台任务。
    pub fn start(config: ClientConfig, events: mpsc::UnboundedSender<ClientEvent>) -> Self {
        let (desired, desired_rx) = watch::channel(None);
        let (commands, commands_rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(run(config, desired_rx, commands_rx, events));
        Self {
            desired,
            commands,
            task,
        }
    }

    /// 提交想要的路由拓扑。修订号由客户端重新分配，传入的值被忽略。
    pub fn apply(&self, topology: RouteTopology) {
        self.desired.send_replace(Some(topology));
    }

    /// 请引擎停止所有流并退出（用户从托盘退出时）。返回引擎是否确认。
    pub async fn shutdown_engine(&self) -> bool {
        let (done, wait) = oneshot::channel();
        if self.commands.send(Command::ShutdownEngine { done }).is_err() {
            return false;
        }
        tokio::time::timeout(SHUTDOWN_ACK_TIMEOUT, wait)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or(false)
    }

    /// 设备探测的句柄：可以复制给其他任务，在控制面的 actor 之外等待应答。
    pub fn prober(&self) -> EngineProber {
        EngineProber {
            commands: self.commands.clone(),
        }
    }

    /// 请引擎立即重新检测离线设备；`node` 为 None 表示全部（选型报告 6.5 节的手动检测）。
    pub fn redetect(&self, node: Option<NodeId>) {
        let _ = self.commands.send(Command::Redetect { node });
    }

    /// 开始录音；`path` 是完整的文件路径。
    pub fn start_recording(&self, node: NodeId, path: String) {
        let _ = self.commands.send(Command::StartRecording { node, path });
    }

    pub fn stop_recording(&self, node: NodeId) {
        let _ = self.commands.send(Command::StopRecording { node });
    }
}

/// 向引擎查询外部设备能力的句柄（[`EngineClient::prober`]）。
#[derive(Debug, Clone)]
pub struct EngineProber {
    commands: mpsc::UnboundedSender<Command>,
}

impl EngineProber {
    /// 查询外部设备的能力（ADR-0014：独占时可选的格式和缓冲）。引擎不在或设备打不开时返回 None。
    pub async fn probe_device(&self, endpoint: EndpointId, direction: EndpointDirection) -> Option<DeviceCapabilities> {
        let (reply, wait) = oneshot::channel();
        self.commands
            .send(Command::ProbeDevice {
                endpoint,
                direction,
                reply,
            })
            .ok()?;
        tokio::time::timeout(PROBE_TIMEOUT, wait).await.ok()?.ok()?
    }
}

impl Drop for EngineClient {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// 跨连接保持的状态。
struct ClientState {
    next_request: u64,
    next_revision: u64,
    /// 已发出、等待应答的拓扑：(请求, 修订号)
    pending_topologies: Vec<(RequestId, TopologyRevision)>,
}

impl ClientState {
    fn request(&mut self) -> RequestId {
        self.next_request += 1;
        RequestId(self.next_request)
    }

    fn revision(&mut self) -> TopologyRevision {
        self.next_revision += 1;
        TopologyRevision(self.next_revision)
    }
}

async fn run(
    config: ClientConfig,
    mut desired: watch::Receiver<Option<RouteTopology>>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: mpsc::UnboundedSender<ClientEvent>,
) {
    let mut state = ClientState {
        next_request: 0,
        next_revision: 0,
        pending_topologies: Vec::new(),
    };
    loop {
        let mut pipe = loop {
            match ClientOptions::new().open(&config.pipe_name) {
                Ok(pipe) => break pipe,
                Err(error)
                    if error.raw_os_error() == Some(ERROR_PIPE_BUSY)
                        || error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => debug!(%error, "cannot open the engine pipe"),
            }
            // 未连接时收到的命令：引擎不在，退出请求答复未确认，探测答复 None，重新检测直接丢弃
            while let Ok(command) = commands.try_recv() {
                match command {
                    Command::ShutdownEngine { done } => {
                        let _ = done.send(false);
                    }
                    Command::ProbeDevice { reply, .. } => {
                        let _ = reply.send(None);
                    }
                    Command::Redetect { .. } | Command::StartRecording { .. } | Command::StopRecording { .. } => {}
                }
            }
            tokio::time::sleep(config.timing.reconnect_interval).await;
        };
        let reason = session(&config, &mut pipe, &mut state, &mut desired, &mut commands, &events).await;
        state.pending_topologies.clear();
        info!(?reason, "engine connection ended");
        if events.send(ClientEvent::Disconnected(reason)).is_err() {
            return;
        }
        tokio::time::sleep(config.timing.reconnect_interval).await;
    }
}

/// 一次连接：握手、下发拓扑、心跳、转发事件，直到断开。
async fn session(
    config: &ClientConfig,
    pipe: &mut NamedPipeClient,
    state: &mut ClientState,
    desired: &mut watch::Receiver<Option<RouteTopology>>,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    events: &mpsc::UnboundedSender<ClientEvent>,
) -> DisconnectReason {
    let mut decoder = FrameDecoder::new();
    let mut buffer = vec![0u8; READ_BUFFER];
    let hello = AppToEngine::Hello {
        protocol: PROTOCOL_VERSION,
        app_version: config.app_version.clone(),
    };
    if send(pipe, &hello).await.is_err() {
        return DisconnectReason::Closed;
    }

    let welcome_deadline = Instant::now() + config.timing.welcome_timeout;
    let mut handshaken = false;
    let mut last_heard = Instant::now();
    let mut ping = tokio::time::interval(config.timing.ping_interval);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut shutdown_waiters: Vec<(RequestId, oneshot::Sender<bool>)> = Vec::new();
    // 断开时随连接一起丢弃，等待方收到"已关闭"，当作 None
    let mut probe_waiters: ProbeWaiters = Vec::new();

    let reason = loop {
        tokio::select! {
            read = pipe.read(&mut buffer) => {
                let count = match read {
                    Ok(0) | Err(_) => break DisconnectReason::Closed,
                    Ok(count) => count,
                };
                last_heard = Instant::now();
                decoder.push(&buffer[..count]);
                let mut outcome = None;
                loop {
                    let message = match decoder.next_message::<EngineToApp>() {
                        Ok(Some(message)) => message,
                        Ok(None) => break,
                        Err(error) => {
                            outcome = Some(DisconnectReason::ProtocolError(error.to_string()));
                            break;
                        }
                    };
                    match message {
                        EngineToApp::Welcome { engine_version, restored_topology, usbip_port, .. } => {
                            info!(%engine_version, ?restored_topology, usbip_port, "connected to the engine");
                            handshaken = true;
                            let _ = events.send(ClientEvent::Connected { restored_topology, usbip_port });
                            // 握手后立即下发当前想要的拓扑（引擎可能刚重启，只恢复了缓存）
                            desired.mark_changed();
                        }
                        EngineToApp::HelloRejected { engine_protocol } => {
                            warn!(engine = engine_protocol.0, app = PROTOCOL_VERSION.0, "protocol version mismatch");
                            outcome = Some(DisconnectReason::VersionMismatch { engine_protocol });
                            break;
                        }
                        message => handle(message, state, &mut shutdown_waiters, &mut probe_waiters, events),
                    }
                }
                if let Some(reason) = outcome {
                    break reason;
                }
            }
            changed = desired.changed(), if handshaken => {
                if changed.is_err() {
                    // 句柄已丢弃
                    break DisconnectReason::Closed;
                }
                let topology = desired.borrow_and_update().clone();
                if let Some(mut topology) = topology {
                    topology.revision = state.revision();
                    let request = state.request();
                    state.pending_topologies.push((request, topology.revision));
                    debug!(revision = topology.revision.0, "sending topology");
                    if send(pipe, &AppToEngine::ApplyTopology { request, topology }).await.is_err() {
                        break DisconnectReason::Closed;
                    }
                }
            }
            command = commands.recv() => {
                let Some(command) = command else {
                    break DisconnectReason::Closed;
                };
                let request = state.request();
                let sent = match command {
                    Command::ShutdownEngine { done } => {
                        let sent = send(pipe, &AppToEngine::Shutdown { request }).await;
                        if sent.is_err() {
                            let _ = done.send(false);
                        } else {
                            shutdown_waiters.push((request, done));
                        }
                        sent
                    }
                    Command::ProbeDevice { endpoint, direction, reply } => {
                        probe_waiters.push((request, reply));
                        send(pipe, &AppToEngine::ProbeDevice { request, endpoint, direction }).await
                    }
                    Command::Redetect { node } => send(pipe, &AppToEngine::Redetect { request, node }).await,
                    Command::StartRecording { node, path } => {
                        send(pipe, &AppToEngine::StartRecording { request, node, path }).await
                    }
                    Command::StopRecording { node } => send(pipe, &AppToEngine::StopRecording { request, node }).await,
                };
                if sent.is_err() {
                    break DisconnectReason::Closed;
                }
            }
            _ = ping.tick() => {
                if !handshaken && Instant::now() >= welcome_deadline {
                    break DisconnectReason::HandshakeTimeout;
                }
                if last_heard.elapsed() > config.timing.unresponsive_after {
                    warn!(silent_for = ?last_heard.elapsed(), "the engine stopped responding");
                    break DisconnectReason::Unresponsive;
                }
                if handshaken {
                    let request = state.request();
                    if send(pipe, &AppToEngine::Ping { request }).await.is_err() {
                        break DisconnectReason::Closed;
                    }
                }
            }
        }
    };
    // 连接断开也算引擎已退出（请求退出后引擎关闭管道是正常结果）
    for (_, done) in shutdown_waiters {
        let _ = done.send(reason == DisconnectReason::Closed);
    }
    reason
}

fn handle(
    message: EngineToApp,
    state: &mut ClientState,
    shutdown_waiters: &mut Vec<(RequestId, oneshot::Sender<bool>)>,
    probe_waiters: &mut ProbeWaiters,
    events: &mpsc::UnboundedSender<ClientEvent>,
) {
    let event = match message {
        EngineToApp::DeviceCapabilities { request, capabilities } => {
            if let Some(index) = probe_waiters.iter().position(|(id, _)| *id == request) {
                let (_, reply) = probe_waiters.swap_remove(index);
                let _ = reply.send(capabilities);
            }
            return;
        }
        EngineToApp::Ack { request } => {
            if let Some(index) = shutdown_waiters.iter().position(|(id, _)| *id == request) {
                let (_, done) = shutdown_waiters.swap_remove(index);
                let _ = done.send(true);
                return;
            }
            match take_pending(state, request) {
                Some(revision) => ClientEvent::TopologyApplied { revision },
                None => return,
            }
        }
        EngineToApp::Rejected {
            request,
            reason,
            detail,
        } => {
            warn!(?reason, %detail, "the engine rejected a command");
            match take_pending(state, request) {
                Some(revision) => ClientEvent::TopologyRejected {
                    revision,
                    reason,
                    detail,
                },
                None => return,
            }
        }
        EngineToApp::EndpointStatus { node, status } => ClientEvent::EndpointStatus { node, status },
        EngineToApp::SlotConnection { slot, connected } => ClientEvent::SlotConnection { slot, connected },
        EngineToApp::SlotFormat { slot, format } => ClientEvent::SlotFormat { slot, format },
        EngineToApp::RecorderStatus { node, status } => ClientEvent::RecorderStatus { node, status },
        EngineToApp::Pong { .. } | EngineToApp::Welcome { .. } | EngineToApp::HelloRejected { .. } => return,
    };
    let _ = events.send(event);
}

fn take_pending(state: &mut ClientState, request: RequestId) -> Option<TopologyRevision> {
    let index = state.pending_topologies.iter().position(|(id, _)| *id == request)?;
    Some(state.pending_topologies.swap_remove(index).1)
}

async fn send(pipe: &mut NamedPipeClient, message: &AppToEngine) -> std::io::Result<()> {
    let bytes = frame::encode(message).map_err(std::io::Error::other)?;
    pipe.write_all(&bytes).await
}
