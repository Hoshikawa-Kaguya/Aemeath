//! 宿主进程的主循环：启动引擎、按缓存恢复出声、在命名管道上依次服务客户端。
//!
//! 始终保留一个等待连接的管道实例（第一个实例要求"首个实例"），避免两次会话之间被别的进程抢先创建同名管道。

use std::future::Future;
use std::io;

use aem_engine::{Engine, EngineConfig, EngineEvent};
use tokio::net::windows::named_pipe::NamedPipeServer;

use crate::config::HostConfig;
use crate::plan_cache::PlanCache;
use crate::session::{self, HostState, SessionEnd};
use crate::status::StatusCache;

/// 运行宿主，直到收到 `shutdown` 命令或 `shutdown` future 完成。
pub async fn run(config: HostConfig, shutdown: impl Future<Output = ()>) -> io::Result<()> {
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        data_dir = %config.data_dir.display(),
        pipe = %config.pipe_name,
        usbip_port = config.usbip_port,
        "engine host starting"
    );
    let mut engine = Engine::start(EngineConfig {
        usbip_port: config.usbip_port,
        identity: config.identity,
    })?;
    let receiver = engine
        .take_events()
        .ok_or_else(|| io::Error::other("engine events were already taken"))?;
    let (sender, events) = tokio::sync::mpsc::unbounded_channel::<EngineEvent>();
    std::thread::Builder::new()
        .name("aem-host-events".to_owned())
        .spawn(move || {
            while let Ok(event) = receiver.recv() {
                if sender.send(event).is_err() {
                    break;
                }
            }
        })?;

    let mut state = HostState {
        engine,
        cache: PlanCache::new(&config.data_dir),
        plan: None,
        status: StatusCache::default(),
        events,
    };
    restore_cached_plan(&mut state);

    let mut listener = aem_sys::pipe::create_pipe_server(&config.pipe_name, true)?;
    tokio::pin!(shutdown);
    loop {
        let connected = tokio::select! {
            () = shutdown.as_mut() => break,
            event = state.events.recv() => {
                match event {
                    Some(event) => state.status.record(&event),
                    None => {
                        tracing::error!("the engine stopped sending events");
                        break;
                    }
                }
                continue;
            }
            connected = listener.connect() => connected,
        };
        if let Err(error) = connected {
            tracing::warn!(%error, "pipe connection failed");
            listener = aem_sys::pipe::create_pipe_server(&config.pipe_name, false)?;
            continue;
        }
        // 先建好下一个等待中的实例，再服务当前客户端
        let mut pipe: NamedPipeServer = std::mem::replace(
            &mut listener,
            aem_sys::pipe::create_pipe_server(&config.pipe_name, false)?,
        );
        tracing::info!("client connected");
        let end = session::serve(&mut state, &mut pipe, &mut shutdown).await;
        tracing::info!(?end, "client session ended");
        if end == SessionEnd::ShutdownRequested {
            break;
        }
    }
    tracing::info!("engine host stopping");
    Ok(())
}

/// 启动时先按缓存的计划恢复出声（ADR-0008），再等主程序下发最新计划。
fn restore_cached_plan(state: &mut HostState) {
    let Some(plan) = state.cache.load() else {
        tracing::info!("no cached plan, starting silent");
        return;
    };
    match state.engine.apply_plan(&plan) {
        Ok(()) => {
            tracing::info!(revision = plan.revision.0, "restored the cached plan");
            state.plan = Some(plan);
        }
        Err(error) => tracing::warn!(%error, "cannot apply the cached plan"),
    }
}
