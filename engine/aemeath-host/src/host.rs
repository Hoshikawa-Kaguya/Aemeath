//! 宿主进程的主循环：启动引擎、按缓存恢复出声、在命名管道上依次服务客户端。
//!
//! 始终保留一个等待连接的管道实例（第一个实例要求"首个实例"），避免两次会话之间被别的进程抢先创建同名管道。

use std::future::Future;
use std::io;

use aemeath_engine::{Engine, EngineConfig, EngineEvent};
use tokio::net::windows::named_pipe::NamedPipeServer;

use crate::meter_publisher;
use crate::session::{self, HostState, SessionEnd};
use crate::status::StatusCache;
use crate::topology_cache::TopologyCache;
use aemeath_runtime::config::RuntimeConfig;

/// 运行宿主，直到收到 `shutdown` 命令或 `shutdown` future 完成。
pub async fn run(config: RuntimeConfig, shutdown: impl Future<Output = ()>) -> io::Result<()> {
    tracing::info!(
        version = aemeath_runtime::VERSION,
        data_dir = %config.data_dir.display(),
        pipe = %config.pipe_name,
        usbip_port = config.usbip_port,
        "engine host starting"
    );
    // 先建管道（要求是首个实例）再起引擎和恢复拓扑：同名管道已被别人占着时直接失败退出，不先占端口或放出声音
    let mut listener = aemeath_sys::pipe::create_pipe_server(&config.pipe_name, true)?;
    let mut engine = start_engine(&config)?;
    let receiver = engine
        .take_events()
        .ok_or_else(|| io::Error::other("engine events were already taken"))?;
    let (sender, events) = tokio::sync::mpsc::unbounded_channel::<EngineEvent>();
    std::thread::Builder::new()
        .name("aemeath-host-events".to_owned())
        .spawn(move || {
            while let Ok(event) = receiver.recv() {
                if sender.send(event).is_err() {
                    break;
                }
            }
        })?;

    let (meter_banks, banks) = tokio::sync::watch::channel(None);
    let mut state = HostState {
        engine,
        cache: TopologyCache::new(&config.data_dir),
        topology: None,
        status: StatusCache::default(),
        events,
        meter_banks,
    };
    restore_cached_topology(&mut state);
    // 电平只用于显示：共享内存建不起来时记录下来，音频照常运行
    let meter_task = match meter_publisher::spawn(&config.meter_memory_name, banks) {
        Ok(task) => Some(task),
        Err(error) => {
            tracing::warn!(%error, "cannot create the meter memory, meters are unavailable");
            None
        }
    };

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
            listener = aemeath_sys::pipe::create_pipe_server(&config.pipe_name, false)?;
            continue;
        }
        // 先建好下一个等待中的实例，再服务当前客户端
        let mut pipe: NamedPipeServer = std::mem::replace(
            &mut listener,
            aemeath_sys::pipe::create_pipe_server(&config.pipe_name, false)?,
        );
        tracing::info!("client connected");
        let end = session::serve(&mut state, &mut pipe, &mut shutdown).await;
        tracing::info!(?end, "client session ended");
        if end == SessionEnd::ShutdownRequested {
            break;
        }
    }
    tracing::info!("engine host stopping");
    if let Some(task) = meter_task {
        task.abort();
    }
    Ok(())
}

/// 启动引擎；未明确指定端口时，若默认端口已被占用或落入系统保留段，改由系统分配空闲端口。
fn start_engine(config: &RuntimeConfig) -> io::Result<Engine> {
    match Engine::start(EngineConfig {
        usbip_port: config.usbip_port,
        identity: config.identity,
    }) {
        Ok(engine) => Ok(engine),
        Err(error) if !config.usbip_port_explicit && config.usbip_port != 0 && is_port_unavailable(&error) => {
            tracing::warn!(
                unavailable = config.usbip_port,
                %error,
                "the default USB/IP port is not available, letting the system pick one"
            );
            Engine::start(EngineConfig {
                usbip_port: 0,
                identity: config.identity,
            })
        }
        Err(error) => Err(error),
    }
}

/// 绑定失败是否说明端口不可用（已被其他进程占用，或落入 Windows 保留端口段返回 10013）。
fn is_port_unavailable(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::AddrInUse | io::ErrorKind::PermissionDenied)
}

/// 启动时先按缓存的拓扑恢复出声，再等主程序下发最新拓扑。
fn restore_cached_topology(state: &mut HostState) {
    let Some(topology) = state.cache.load() else {
        tracing::info!("no cached topology, starting silent");
        return;
    };
    match state.engine.apply_topology(&topology) {
        Ok(()) => {
            tracing::info!(revision = topology.revision.0, "restored the cached topology");
            state.publish_meters();
            state.topology = Some(topology);
        }
        Err(error) => tracing::warn!(%error, "cannot apply the cached topology"),
    }
}
