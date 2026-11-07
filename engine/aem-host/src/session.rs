//! 一个客户端（主程序）连接的协议处理。
//!
//! 规则（`aem-protocol` 的 README）：
//! - 连接后 [`HELLO_TIMEOUT`] 内必须先发 `hello`，协议版本完全相同才回 `welcome`，随后发一份状态快照；
//!   握手前收到的其他命令回 `rejected`（`not_handshaken`）。
//! - 同一连接内，计划的修订号必须递增（主程序重启后修订号会从头开始，所以只在同一连接内比较）。
//! - 引擎事件随时推送；没有客户端时只更新状态缓存。

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use aem_engine::{Engine, EngineEvent};
use aem_protocol::PROTOCOL_VERSION;
use aem_protocol::frame::{self, FrameDecoder};
use aem_protocol::ids::{PlanRevision, RequestId};
use aem_protocol::message::{AppToEngine, EngineToApp, RejectReason};
use aem_protocol::plan::{PlanError, RoutePlan};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::NamedPipeServer;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::plan_cache::PlanCache;
use crate::status::{StatusCache, to_messages};

/// 连接后必须在这段时间内完成握手
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
const READ_BUFFER: usize = 64 * 1024;
const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 宿主进程的状态：引擎、计划缓存、当前计划、状态缓存和引擎事件。
pub struct HostState {
    pub engine: Engine,
    pub cache: PlanCache,
    /// 引擎正在执行的计划（来自缓存或主程序）
    pub plan: Option<RoutePlan>,
    pub status: StatusCache,
    pub events: UnboundedReceiver<EngineEvent>,
}

impl HostState {
    /// 下发计划并写入缓存（缓存写入失败只记录，不影响出声）。
    fn apply(&mut self, plan: RoutePlan) -> Result<(), PlanError> {
        self.engine.apply_plan(&plan)?;
        if let Err(error) = self.cache.save(&plan) {
            tracing::warn!(%error, "cannot cache the plan");
        }
        tracing::info!(
            revision = plan.revision.0,
            nodes = plan.nodes.len(),
            routes = plan.routes.len(),
            "plan applied"
        );
        self.plan = Some(plan);
        Ok(())
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
    last_revision: Option<PlanRevision>,
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
                    for message in to_messages(&event, state.plan.as_ref()) {
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
                restored_plan: state.plan.as_ref().map(|plan| plan.revision),
            };
            if send(pipe, &welcome).await.is_err() {
                return Flow::Disconnect;
            }
            for message in state.status.snapshot(state.plan.as_ref()) {
                if send(pipe, &message).await.is_err() {
                    return Flow::Disconnect;
                }
            }
            return Flow::Continue;
        }
        AppToEngine::ApplyPlan { request, .. } | AppToEngine::Ping { request } | AppToEngine::Shutdown { request }
            if !session.handshaken =>
        {
            rejected(request, RejectReason::NotHandshaken, "say hello first".to_owned())
        }
        AppToEngine::ApplyPlan { request, plan } => {
            if let Some(last) = session.last_revision
                && plan.revision <= last
            {
                rejected(
                    request,
                    RejectReason::StalePlan,
                    format!("revision {} is not newer than {}", plan.revision.0, last.0),
                )
            } else {
                let revision = plan.revision;
                match state.apply(plan) {
                    Ok(()) => {
                        session.last_revision = Some(revision);
                        EngineToApp::Ack { request }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "rejected an invalid plan");
                        rejected(request, RejectReason::InvalidPlan, error.to_string())
                    }
                }
            }
        }
        AppToEngine::Ping { request } => EngineToApp::Pong { request },
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
