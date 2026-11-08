//! 一个客户端（主程序）连接的协议处理。
//!
//! 规则（`aemeath-protocol` 的 README）：
//! - 连接后 [`HELLO_TIMEOUT`] 内必须先发 `hello`，协议版本完全相同才回 `welcome`，随后发一份状态快照；
//!   握手前收到的其他命令回 `rejected`（`not_handshaken`）。
//! - 同一连接内，拓扑的修订号必须递增（主程序重启后修订号会从头开始，所以只在同一连接内比较）。
//! - 引擎事件随时推送；没有客户端时只更新状态缓存。

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use aemeath_engine::meters::MeterBank;
use aemeath_engine::{Engine, EngineEvent};
use aemeath_io::redetect::RedetectTarget;
use aemeath_protocol::PROTOCOL_VERSION;
use aemeath_protocol::frame::{self, FrameDecoder};
use aemeath_protocol::ids::{NodeId, RequestId, TopologyRevision};
use aemeath_protocol::message::{AppToEngine, EngineToApp, RejectReason};
use aemeath_protocol::topology::{RouteTopology, TopologyError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::NamedPipeServer;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::watch;

use crate::status::{StatusCache, to_messages};
use crate::topology_cache::TopologyCache;

/// 连接后必须在这段时间内完成握手
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
const READ_BUFFER: usize = 64 * 1024;
const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 宿主进程的状态：引擎、拓扑缓存、当前拓扑、状态缓存和引擎事件。
pub struct HostState {
    pub engine: Engine,
    pub cache: TopologyCache,
    /// 引擎正在执行的拓扑（来自缓存或主程序）
    pub topology: Option<RouteTopology>,
    pub status: StatusCache,
    pub events: UnboundedReceiver<EngineEvent>,
    /// 当前拓扑的电平组，交给电平转写任务（[`crate::meter_publisher`]）
    pub meter_banks: watch::Sender<Option<Arc<MeterBank>>>,
}

impl HostState {
    /// 下发拓扑并写入缓存（缓存写入失败只记录，不影响出声）。
    fn apply(&mut self, topology: RouteTopology) -> Result<(), TopologyError> {
        self.engine.apply_topology(&topology)?;
        self.publish_meters();
        if let Err(error) = self.cache.save(&topology) {
            tracing::warn!(%error, "cannot cache the topology");
        }
        tracing::info!(
            revision = topology.revision.0,
            nodes = topology.nodes.len(),
            routes = topology.routes.len(),
            "topology applied"
        );
        self.topology = Some(topology);
        Ok(())
    }

    /// 立即重新检测离线的外部设备和缺失的程序：`node` 为 None 时全部，否则只有这个节点的设备或程序。
    fn redetect(&self, node: Option<NodeId>) {
        let targets = self.topology.as_ref().map(|topology| {
            topology
                .nodes
                .iter()
                .filter(|candidate| node.is_none_or(|id| id == candidate.id))
                .filter_map(|candidate| match candidate.kind.program() {
                    Some(program) => Some(RedetectTarget::Program(program.clone())),
                    None => candidate.kind.device().cloned().map(RedetectTarget::Endpoint),
                })
                .collect::<Vec<_>>()
        });
        match targets {
            Some(targets) if node.is_some() => self.engine.redetect(Some(&targets)),
            _ => self.engine.redetect(None),
        }
    }

    /// 换拓扑后，把引擎新的电平组交给转写任务。
    pub fn publish_meters(&self) {
        self.meter_banks.send_replace(self.engine.meters().cloned());
    }
}

/// 会话结束的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEnd {
    /// 客户端断开或协议出错：等待下一个连接
    Disconnected,
    /// 收到 `shutdown` 命令或退出信号，或者引擎已经停止：宿主进程退出
    ShutdownRequested,
}

enum Flow {
    Continue,
    Disconnect,
    Shutdown,
}

struct Session {
    handshaken: bool,
    last_revision: Option<TopologyRevision>,
}

pub async fn serve<F: Future<Output = ()>>(
    state: &mut HostState,
    pipe: &mut NamedPipeServer,
    shutdown: &mut Pin<&mut F>,
) -> SessionEnd {
    let mut session = Session {
        handshaken: false,
        last_revision: None,
    };
    let mut decoder = FrameDecoder::new();
    let mut buffer = vec![0u8; READ_BUFFER];
    let hello_deadline = tokio::time::sleep(HELLO_TIMEOUT);
    tokio::pin!(hello_deadline);

    loop {
        tokio::select! {
            () = shutdown.as_mut() => return SessionEnd::ShutdownRequested,
            () = &mut hello_deadline, if !session.handshaken => {
                tracing::warn!("client did not say hello in time, disconnecting");
                return SessionEnd::Disconnected;
            }
            event = state.events.recv() => {
                let Some(event) = event else {
                    tracing::error!("the engine stopped sending events");
                    return SessionEnd::ShutdownRequested;
                };
                state.status.record(&event);
                if session.handshaken {
                    for message in to_messages(&event, state.topology.as_ref()) {
                        if send(pipe, &message).await.is_err() {
                            return SessionEnd::Disconnected;
                        }
                    }
                }
            }
            read = pipe.read(&mut buffer) => {
                let count = match read {
                    Ok(0) => return SessionEnd::Disconnected,
                    Ok(count) => count,
                    Err(error) => {
                        tracing::info!(%error, "client connection closed");
                        return SessionEnd::Disconnected;
                    }
                };
                decoder.push(&buffer[..count]);
                loop {
                    let message = match decoder.next_message::<AppToEngine>() {
                        Ok(Some(message)) => message,
                        Ok(None) => break,
                        Err(error) => {
                            tracing::warn!(%error, "invalid message from the client, disconnecting");
                            return SessionEnd::Disconnected;
                        }
                    };
                    match handle(state, &mut session, pipe, message).await {
                        Flow::Continue => {}
                        Flow::Disconnect => return SessionEnd::Disconnected,
                        Flow::Shutdown => return SessionEnd::ShutdownRequested,
                    }
                }
            }
        }
    }
}

async fn handle(
    state: &mut HostState,
    session: &mut Session,
    pipe: &mut NamedPipeServer,
    message: AppToEngine,
) -> Flow {
    let reply = match message {
        AppToEngine::Hello { protocol, app_version } => {
            if protocol != PROTOCOL_VERSION {
                tracing::warn!(client = protocol.0, engine = PROTOCOL_VERSION.0, %app_version, "protocol version mismatch");
                let _ = send(
                    pipe,
                    &EngineToApp::HelloRejected {
                        engine_protocol: PROTOCOL_VERSION,
                    },
                )
                .await;
                return Flow::Disconnect;
            }
            tracing::info!(%app_version, "client said hello");
            session.handshaken = true;
            let welcome = EngineToApp::Welcome {
                protocol: PROTOCOL_VERSION,
                engine_version: ENGINE_VERSION.to_owned(),
                restored_topology: state.topology.as_ref().map(|topology| topology.revision),
            };
            if send(pipe, &welcome).await.is_err() {
                return Flow::Disconnect;
            }
            for message in state.status.snapshot(state.topology.as_ref()) {
                if send(pipe, &message).await.is_err() {
                    return Flow::Disconnect;
                }
            }
            return Flow::Continue;
        }
        AppToEngine::ApplyTopology { request, .. }
        | AppToEngine::Ping { request }
        | AppToEngine::Shutdown { request }
        | AppToEngine::ProbeDevice { request, .. }
        | AppToEngine::Redetect { request, .. }
        | AppToEngine::StartRecording { request, .. }
        | AppToEngine::StopRecording { request, .. }
            if !session.handshaken =>
        {
            rejected(request, RejectReason::NotHandshaken, "say hello first".to_owned())
        }
        AppToEngine::ApplyTopology { request, topology } => {
            if let Some(last) = session.last_revision
                && topology.revision <= last
            {
                rejected(
                    request,
                    RejectReason::StaleTopology,
                    format!("revision {} is not newer than {}", topology.revision.0, last.0),
                )
            } else {
                let revision = topology.revision;
                match state.apply(topology) {
                    Ok(()) => {
                        session.last_revision = Some(revision);
                        EngineToApp::Ack { request }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "rejected an invalid topology");
                        rejected(request, RejectReason::InvalidTopology, error.to_string())
                    }
                }
            }
        }
        AppToEngine::Ping { request } => EngineToApp::Pong { request },
        AppToEngine::ProbeDevice {
            request,
            endpoint,
            direction,
        } => {
            // 探测要做几十次 COM 调用（约 100 ms），放到阻塞线程上，不卡住管道和心跳
            let probe = state.engine.device_prober();
            let capabilities = tokio::task::spawn_blocking(move || probe.probe(&endpoint, direction))
                .await
                .ok()
                .flatten();
            EngineToApp::DeviceCapabilities { request, capabilities }
        }
        AppToEngine::Redetect { request, node } => {
            state.redetect(node);
            EngineToApp::Ack { request }
        }
        AppToEngine::StartRecording { request, node, path } => {
            recording_reply(request, node, state.engine.start_recording(node, PathBuf::from(path)))
        }
        AppToEngine::StopRecording { request, node } => {
            recording_reply(request, node, state.engine.stop_recording(node))
        }
        AppToEngine::Shutdown { request } => {
            tracing::info!("shutdown requested by the client");
            let _ = send(pipe, &EngineToApp::Ack { request }).await;
            return Flow::Shutdown;
        }
    };
    if send(pipe, &reply).await.is_err() {
        Flow::Disconnect
    } else {
        Flow::Continue
    }
}

fn recording_reply(request: RequestId, node: NodeId, accepted: bool) -> EngineToApp {
    if accepted {
        EngineToApp::Ack { request }
    } else {
        rejected(request, RejectReason::NotARecorder, format!("{node} is not a recorder"))
    }
}

fn rejected(request: RequestId, reason: RejectReason, detail: String) -> EngineToApp {
    EngineToApp::Rejected {
        request,
        reason,
        detail,
    }
}

async fn send(pipe: &mut NamedPipeServer, message: &EngineToApp) -> std::io::Result<()> {
    let bytes = frame::encode(message).map_err(std::io::Error::other)?;
    pipe.write_all(&bytes).await
}
