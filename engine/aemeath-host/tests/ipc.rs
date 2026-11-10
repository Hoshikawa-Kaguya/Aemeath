//! 集成测试：在同一进程里启动宿主，用命名管道客户端按协议对话。
//!
//! USB/IP 服务端用系统分配的端口，管道名和数据目录都是测试专用的，不影响正在运行的引擎。

// 整个文件都是测试代码：出错时直接让测试失败（clippy 的"测试中允许"只覆盖 #[test] 函数，不含这里的辅助函数）
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use aemeath_protocol::PROTOCOL_VERSION;
use aemeath_protocol::frame::{self, FrameDecoder};
use aemeath_protocol::ids::{NodeId, ProtocolVersion, RequestId, TopologyRevision};
use aemeath_protocol::message::{AppToEngine, EngineToApp, RejectReason};
use aemeath_protocol::topology::{NodeKind, Resampler, RouteTopology, Strip, TopologyNode};
use aemeath_protocol::units::SampleRate;
use aemeath_runtime::config::RuntimeConfig;
use aemeath_usbip::identity::IdentityProfile;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};

const TIMEOUT: Duration = Duration::from_secs(10);

static NEXT_TEST: AtomicU32 = AtomicU32::new(0);

fn config(data_dir: PathBuf) -> RuntimeConfig {
    let test = NEXT_TEST.fetch_add(1, Ordering::Relaxed);
    RuntimeConfig {
        data_dir,
        usbip_port: 0,
        identity: IdentityProfile::Development,
        pipe_name: format!(r"\\.\pipe\aemeath-host-test-{}-{test}", std::process::id()),
        meter_memory_name: format!(r"Local\aemeath-host-test-meters-{}-{test}", std::process::id()),
        autostart: false,
    }
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("aemeath-host-ipc-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn topology(revision: u64) -> RouteTopology {
    RouteTopology {
        revision: TopologyRevision(revision),
        engine_sample_rate: SampleRate::HZ_48000,
        clock_master: None,
        resampler: Resampler::default(),
        nodes: vec![TopologyNode {
            id: NodeId(1),
            kind: NodeKind::Bus,
            strip: Strip::default(),
            slot_settings: None,
            io: None,
        }],
        routes: vec![],
    }
}

struct Client {
    pipe: NamedPipeClient,
    decoder: FrameDecoder,
}

impl Client {
    /// 连接宿主；宿主刚启动、管道还没建好时重试。
    async fn connect(name: &str) -> Client {
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        loop {
            match ClientOptions::new().open(name) {
                Ok(pipe) => {
                    return Client {
                        pipe,
                        decoder: FrameDecoder::new(),
                    };
                }
                Err(_) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(error) => panic!("cannot connect to {name}: {error}"),
            }
        }
    }

    async fn send(&mut self, message: &AppToEngine) {
        self.pipe.write_all(&frame::encode(message).unwrap()).await.unwrap();
    }

    async fn receive(&mut self) -> EngineToApp {
        let mut buffer = [0u8; 4096];
        loop {
            if let Some(message) = self.decoder.next_message::<EngineToApp>().unwrap() {
                return message;
            }
            let read = tokio::time::timeout(TIMEOUT, self.pipe.read(&mut buffer))
                .await
                .expect("timed out waiting for the host")
                .unwrap();
            assert!(read > 0, "the host closed the connection");
            self.decoder.push(&buffer[..read]);
        }
    }

    async fn hello(&mut self) -> EngineToApp {
        self.send(&AppToEngine::Hello {
            protocol: PROTOCOL_VERSION,
            app_version: "test".to_owned(),
        })
        .await;
        self.receive().await
    }

    async fn request(&mut self, message: AppToEngine) -> EngineToApp {
        self.send(&message).await;
        self.receive().await
    }
}

#[tokio::test]
async fn full_session_and_restart_from_the_cached_topology() {
    let dir = temp_dir("session");
    let config = config(dir.clone());
    let pipe = config.pipe_name.clone();
    let host = tokio::spawn(aemeath_host::run(config, std::future::pending()));

    let mut client = Client::connect(&pipe).await;
    // 握手前的命令被拒绝
    assert_eq!(
        client.request(AppToEngine::Ping { request: RequestId(1) }).await,
        EngineToApp::Rejected {
            request: RequestId(1),
            reason: RejectReason::NotHandshaken,
            detail: "say hello first".to_owned(),
        }
    );
    let EngineToApp::Welcome { restored_topology, .. } = client.hello().await else {
        panic!("expected welcome");
    };
    assert_eq!(restored_topology, None);

    assert_eq!(
        client.request(AppToEngine::Ping { request: RequestId(2) }).await,
        EngineToApp::Pong { request: RequestId(2) }
    );
    assert_eq!(
        client
            .request(AppToEngine::ApplyTopology {
                request: RequestId(3),
                topology: topology(7),
            })
            .await,
        EngineToApp::Ack { request: RequestId(3) }
    );
    let stale = client
        .request(AppToEngine::ApplyTopology {
            request: RequestId(4),
            topology: topology(7),
        })
        .await;
    assert!(matches!(
        stale,
        EngineToApp::Rejected {
            reason: RejectReason::StaleTopology,
            ..
        }
    ));
    let mut invalid = topology(8);
    invalid.nodes.push(invalid.nodes[0].clone());
    let rejected = client
        .request(AppToEngine::ApplyTopology {
            request: RequestId(5),
            topology: invalid,
        })
        .await;
    assert!(matches!(
        rejected,
        EngineToApp::Rejected {
            reason: RejectReason::InvalidTopology,
            ..
        }
    ));
    assert_eq!(
        client.request(AppToEngine::Shutdown { request: RequestId(6) }).await,
        EngineToApp::Ack { request: RequestId(6) }
    );
    tokio::time::timeout(TIMEOUT, host).await.unwrap().unwrap().unwrap();

    // 重启后先按缓存恢复，并在 welcome 中报告
    let config = self::config(dir.clone());
    let pipe = config.pipe_name.clone();
    let host = tokio::spawn(aemeath_host::run(config, std::future::pending()));
    let mut client = Client::connect(&pipe).await;
    let EngineToApp::Welcome { restored_topology, .. } = client.hello().await else {
        panic!("expected welcome");
    };
    assert_eq!(restored_topology, Some(TopologyRevision(7)));
    client.request(AppToEngine::Shutdown { request: RequestId(1) }).await;
    tokio::time::timeout(TIMEOUT, host).await.unwrap().unwrap().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_protocol_mismatch_is_rejected_and_the_host_keeps_serving() {
    let dir = temp_dir("mismatch");
    let config = config(dir.clone());
    let pipe = config.pipe_name.clone();
    let host = tokio::spawn(aemeath_host::run(config, std::future::pending()));

    let mut client = Client::connect(&pipe).await;
    client
        .send(&AppToEngine::Hello {
            protocol: ProtocolVersion(PROTOCOL_VERSION.0 + 1),
            app_version: "future".to_owned(),
        })
        .await;
    assert_eq!(
        client.receive().await,
        EngineToApp::HelloRejected {
            engine_protocol: PROTOCOL_VERSION,
        }
    );

    // 断开后宿主继续等待下一个客户端
    let mut client = Client::connect(&pipe).await;
    assert!(matches!(client.hello().await, EngineToApp::Welcome { .. }));
    client.request(AppToEngine::Shutdown { request: RequestId(1) }).await;
    tokio::time::timeout(TIMEOUT, host).await.unwrap().unwrap().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_second_host_with_the_same_pipe_name_fails() {
    let dir = temp_dir("twice");
    let config = config(dir.clone());
    let pipe = config.pipe_name.clone();
    let first = tokio::spawn(aemeath_host::run(config.clone(), std::future::pending()));
    let mut client = Client::connect(&pipe).await;
    assert!(matches!(client.hello().await, EngineToApp::Welcome { .. }));

    let second = aemeath_host::run(config, std::future::pending()).await;
    assert!(second.is_err(), "the pipe name is taken by the first host");

    client.request(AppToEngine::Shutdown { request: RequestId(1) }).await;
    tokio::time::timeout(TIMEOUT, first).await.unwrap().unwrap().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn meters_follow_the_applied_topology() {
    use aemeath_protocol::meters::{METER_MEMORY_WORDS, MeterReader};
    use aemeath_sys::shared_memory::SharedMemory;

    let dir = temp_dir("meters");
    let config = config(dir.clone());
    let pipe = config.pipe_name.clone();
    let memory_name = config.meter_memory_name.clone();
    let host = tokio::spawn(aemeath_host::run(config, std::future::pending()));

    let mut client = Client::connect(&pipe).await;
    assert!(matches!(client.hello().await, EngineToApp::Welcome { .. }));
    let ack = client
        .request(AppToEngine::ApplyTopology {
            request: RequestId(1),
            topology: topology(1),
        })
        .await;
    assert_eq!(ack, EngineToApp::Ack { request: RequestId(1) });

    // 转写任务按自己的节奏更新节点表
    let memory = SharedMemory::open(&memory_name, METER_MEMORY_WORDS).unwrap();
    let reader = MeterReader::new(memory.words()).unwrap();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        if let Some(meters) = reader.take()
            && meters.iter().map(|meter| meter.node).eq([NodeId(1)])
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "meters never showed the topology"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    host.abort();
    let _ = std::fs::remove_dir_all(&dir);
}
