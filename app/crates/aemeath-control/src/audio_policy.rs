//! Windows 音频策略：应用页的会话与按应用指派，工具页的系统默认设备。
//!
//! 线程模型：COM 对象只能在创建它的线程上使用，所以由一个专用线程持有（MTA），其他代码经请求通道访问。
//! 按应用指派的激活工厂第一次使用时才创建（创建失败只影响应用页）。

use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;

use aemeath_sys::app_routing::AppRouting;
use aemeath_sys::audio_endpoints::{AudioEndpoints, AudioFlow, EndpointState};
use aemeath_sys::audio_sessions::render_sessions;
use aemeath_sys::com::{ComApartment, ComError};
use aemeath_sys::policy_config::{DefaultRole, PolicyConfig};
use thiserror::Error;
use tokio::sync::oneshot;
use tracing::info;

/// 一个正在出声的程序（音频会话）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppSession {
    pub process_id: u32,
    pub executable: Option<PathBuf>,
    /// 程序设的会话显示名；没有时为空
    pub display_name: String,
    /// 会话当前所在的播放端点（落点）
    pub endpoint_id: String,
    pub system_sounds: bool,
}

/// 一个活动端点，以及它是不是 Windows 的默认设备、默认通信设备。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointEntry {
    pub id: String,
    pub name: String,
    pub is_default: bool,
    pub is_communications: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("audio policy call failed: {0}")]
pub struct PolicyError(String);

impl From<ComError> for PolicyError {
    fn from(error: ComError) -> Self {
        Self(error.to_string())
    }
}

type Reply<T> = oneshot::Sender<Result<T, PolicyError>>;

enum Request {
    Sessions(Reply<Vec<AppSession>>),
    SetLanding {
        process_id: u32,
        endpoint_id: Option<String>,
        reply: Reply<()>,
    },
    Landing {
        process_id: u32,
        reply: Reply<Option<String>>,
    },
    Endpoints {
        flow: AudioFlow,
        reply: Reply<Vec<EndpointEntry>>,
    },
    SetDefault {
        endpoint_id: String,
        role: DefaultRole,
        reply: Reply<()>,
    },
}

/// 策略线程的句柄。丢弃时线程退出。
#[derive(Debug, Clone)]
pub struct AudioPolicy {
    requests: std_mpsc::Sender<Request>,
}

impl AudioPolicy {
    /// 启动专用的 COM 线程，等它初始化成功后返回。之后所有调用都排进这个线程执行，调用方不必关心 COM。
    pub fn start() -> Result<Self, PolicyError> {
        let (requests, receiver) = std_mpsc::channel();
        let (ready, started) = std_mpsc::channel();
        std::thread::Builder::new()
            .name("aemeath-audio-policy".to_owned())
            .spawn(move || worker(receiver, ready))
            .map_err(|error| PolicyError(error.to_string()))?;
        started.recv().map_err(|error| PolicyError(error.to_string()))??;
        Ok(Self { requests })
    }

    /// 所有活动播放端点上的会话。
    pub async fn sessions(&self) -> Result<Vec<AppSession>, PolicyError> {
        self.ask(Request::Sessions).await
    }

    /// 把进程的默认播放端点指派为 `endpoint_id`；None 表示恢复为跟随系统默认。进程须正在运行并有音频会话。
    pub async fn set_landing(&self, process_id: u32, endpoint_id: Option<String>) -> Result<(), PolicyError> {
        self.ask(|reply| Request::SetLanding {
            process_id,
            endpoint_id,
            reply,
        })
        .await
    }

    /// 进程当前的持久指派；没有指派时为 None。进程须正在运行并有音频会话。
    pub async fn landing(&self, process_id: u32) -> Result<Option<String>, PolicyError> {
        self.ask(|reply| Request::Landing { process_id, reply }).await
    }

    /// 某个方向的全部活动端点（包括 Aemeath 的槽位），标出当前的默认设备和默认通信设备。
    pub async fn endpoints(&self, flow: AudioFlow) -> Result<Vec<EndpointEntry>, PolicyError> {
        self.ask(|reply| Request::Endpoints { flow, reply }).await
    }

    /// 把端点设为 Windows 的默认设备或默认通信设备（工具页）。
    pub async fn set_default(&self, endpoint_id: String, role: DefaultRole) -> Result<(), PolicyError> {
        self.ask(|reply| Request::SetDefault {
            endpoint_id,
            role,
            reply,
        })
        .await
    }

    async fn ask<T>(&self, request: impl FnOnce(Reply<T>) -> Request) -> Result<T, PolicyError> {
        let (reply, wait) = oneshot::channel();
        self.requests.send(request(reply)).map_err(|_| stopped())?;
        wait.await.map_err(|_| stopped())?
    }
}

fn stopped() -> PolicyError {
    PolicyError("the audio policy thread stopped".to_owned())
}

struct Worker {
    endpoints: AudioEndpoints,
    policy: PolicyConfig,
    /// 第一次指派时才创建
    routing: Option<AppRouting>,
}

/// COM 线程的主体：建好 COM 对象后报告就绪，然后逐个执行请求，直到请求通道关闭。
fn worker(requests: std_mpsc::Receiver<Request>, ready: std_mpsc::Sender<Result<(), PolicyError>>) {
    let setup = || -> Result<(ComApartment, Worker), PolicyError> {
        let com = ComApartment::multithreaded()?;
        let worker = Worker {
            endpoints: AudioEndpoints::new()?,
            policy: PolicyConfig::new()?,
            routing: None,
        };
        Ok((com, worker))
    };
    // 注意：Worker 必须先于 ComApartment 释放（COM 对象要在退出套间之前释放）
    let (_com, mut worker) = match setup() {
        Ok(parts) => parts,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    while let Ok(request) = requests.recv() {
        match request {
            Request::Sessions(reply) => {
                let _ = reply.send(worker.sessions());
            }
            Request::SetLanding {
                process_id,
                endpoint_id,
                reply,
            } => {
                let _ = reply.send(worker.set_landing(process_id, endpoint_id.as_deref()));
            }
            Request::Landing { process_id, reply } => {
                let _ = reply.send(worker.landing(process_id));
            }
            Request::Endpoints { flow, reply } => {
                let _ = reply.send(worker.list(flow));
            }
            Request::SetDefault {
                endpoint_id,
                role,
                reply,
            } => {
                let result = worker
                    .policy
                    .set_default_endpoint(&endpoint_id, role)
                    .map_err(PolicyError::from);
                if result.is_ok() {
                    info!(endpoint_id, ?role, "Windows default device set");
                }
                let _ = reply.send(result);
            }
        }
    }
    drop(worker);
}

impl Worker {
    fn sessions(&self) -> Result<Vec<AppSession>, PolicyError> {
        Ok(render_sessions()?
            .into_iter()
            .map(|session| AppSession {
                process_id: session.process_id,
                executable: session.executable,
                display_name: session.display_name,
                endpoint_id: session.endpoint_id,
                system_sounds: session.system_sounds,
            })
            .collect())
    }

    /// 用到时才创建按应用指派的接口：它是未公开接口，建不起来时不影响其他功能。
    fn routing(&mut self) -> Result<&AppRouting, PolicyError> {
        Ok(match &mut self.routing {
            Some(routing) => routing,
            slot => slot.insert(AppRouting::new()?),
        })
    }

    fn set_landing(&mut self, process_id: u32, endpoint_id: Option<&str>) -> Result<(), PolicyError> {
        self.routing()?.set_render_endpoint(process_id, endpoint_id)?;
        info!(process_id, endpoint_id, "app landing set");
        Ok(())
    }

    fn landing(&mut self, process_id: u32) -> Result<Option<String>, PolicyError> {
        Ok(self.routing()?.render_endpoint(process_id)?)
    }

    fn list(&self, flow: AudioFlow) -> Result<Vec<EndpointEntry>, PolicyError> {
        let default = self.endpoints.default_endpoint(flow, DefaultRole::Default)?;
        let communications = self.endpoints.default_endpoint(flow, DefaultRole::Communications)?;
        let mut entries: Vec<EndpointEntry> = self
            .endpoints
            .list(flow)?
            .into_iter()
            .filter(|info| info.state == EndpointState::Active)
            .map(|info| EndpointEntry {
                is_default: default.as_deref() == Some(info.id.as_str()),
                is_communications: communications.as_deref() == Some(info.id.as_str()),
                name: info.name.unwrap_or_else(|| info.id.clone()),
                id: info.id,
            })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }
}
