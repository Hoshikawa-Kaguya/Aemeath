//! IPC 客户端对照一个假的引擎服务端测试：握手、下发拓扑、断线重连、心跳超时、协议版本不符。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use aemeath_control::engine_client::{ClientConfig, ClientEvent, DisconnectReason, EngineClient, Timing};
use aemeath_protocol::PROTOCOL_VERSION;
use aemeath_protocol::frame::{self, FrameDecoder};
use aemeath_protocol::ids::{NodeId, ProtocolVersion, TopologyRevision};
use aemeath_protocol::message::{AppToEngine, EngineToApp};
use aemeath_protocol::topology::{NodeKind, Resampler, RouteTopology, Strip, TopologyNode};
use aemeath_protocol::units::SampleRate;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::mpsc;

const EVENT_TIMEOUT: Duration = Duration::from_secs(5);

fn fast_timing() -> Timing {
    Timing {
        reconnect_interval: Duration::from_millis(50),
        welcome_timeout: Duration::from_millis(500),
        ping_interval: Duration::from_millis(100),
        unresponsive_after: Duration::from_millis(400),
    }
}

fn pipe_name(test: &str) -> String {
    format!(r"\\.\pipe\aemeath-control-test-{test}-{}", std::process::id())
}

fn start(name: &str) -> (EngineClient, mpsc::UnboundedReceiver<ClientEvent>) {
    let (events, receiver) = mpsc::unbounded_channel();
    let client = EngineClient::start(
        ClientConfig {
            pipe_name: name.to_owned(),
            app_version: "test".to_owned(),
            timing: fast_timing(),
        },
        events,
    );
    (client, receiver)
}

fn topology(bus: u32) -> RouteTopology {
    RouteTopology {
        revision: TopologyRevision(0),
        engine_sample_rate: SampleRate::HZ_48000,
        clock_master: None,
        resampler: Resampler::default(),
        nodes: vec![TopologyNode {
            id: NodeId(bus),
            kind: NodeKind::Bus,
            strip: Strip::default(),
            io: None,
        }],
        routes: Vec::new(),
    }
}

/// 假引擎的一个连接。
struct FakeEngine {
    pipe: NamedPipeServer,
    decoder: FrameDecoder,
}

impl FakeEngine {
    async fn accept(name: &str, first: bool) -> Self {
        let pipe = ServerOptions::new().first_pipe_instance(first).create(name).unwrap();
        pipe.connect().await.unwrap();
        Self {
            pipe,
            decoder: FrameDecoder::new(),
        }
    }

    async fn receive(&mut self) -> AppToEngine {
        let mut buffer = [0u8; 4096];
        loop {
            if let Some(message) = self.decoder.next_message().unwrap() {
                return message;
            }
            let count = tokio::time::timeout(EVENT_TIMEOUT, self.pipe.read(&mut buffer))
                .await
                .unwrap()
                .unwrap();
            assert!(count > 0, "client closed the pipe");
            self.decoder.push(&buffer[..count]);
        }
    }

    /// 跳过心跳，取下一条其他消息
    async fn receive_command(&mut self) -> AppToEngine {
        loop {
            match self.receive().await {
                AppToEngine::Ping { request } => self.send(&EngineToApp::Pong { request }).await,
                message => return message,
            }
        }
    }

    async fn send(&mut self, message: &EngineToApp) {
        self.pipe.write_all(&frame::encode(message).unwrap()).await.unwrap();
    }

    async fn handshake(&mut self) {
        let AppToEngine::Hello { protocol, .. } = self.receive().await else {
            panic!("expected hello");
        };
        assert_eq!(protocol, PROTOCOL_VERSION);
        self.send(&EngineToApp::Welcome {
            protocol: PROTOCOL_VERSION,
            engine_version: "fake".to_owned(),
            restored_topology: None,
        })
        .await;
    }
}

async fn next_event(events: &mut mpsc::UnboundedReceiver<ClientEvent>) -> ClientEvent {
    tokio::time::timeout(EVENT_TIMEOUT, events.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn sends_the_latest_topology_after_the_handshake_and_reports_the_result() {
    let name = pipe_name("apply");
    let server = tokio::spawn({
        let name = name.clone();
        async move {
            let mut engine = FakeEngine::accept(&name, true).await;
            engine.handshake().await;
            let AppToEngine::ApplyTopology { request, topology } = engine.receive_command().await else {
                panic!("expected a topology");
            };
            assert_eq!(topology.nodes[0].id, NodeId(2), "only the latest topology is sent");
            assert_eq!(topology.revision, TopologyRevision(1));
            engine.send(&EngineToApp::Ack { request }).await;
            engine
        }
    });
    let (client, mut events) = start(&name);
    // 连接前提交两份：只有最后一份会被下发
    client.apply(topology(1));
    client.apply(topology(2));

    assert_eq!(
        next_event(&mut events).await,
        ClientEvent::Connected {
            restored_topology: None
        }
    );
    assert_eq!(
        next_event(&mut events).await,
        ClientEvent::TopologyApplied {
            revision: TopologyRevision(1)
        }
    );
    drop(server.await.unwrap());
}

#[tokio::test]
async fn reconnects_and_resends_the_topology_with_a_newer_revision() {
    let name = pipe_name("reconnect");
    let (client, mut events) = start(&name);
    client.apply(topology(7));

    let mut engine = FakeEngine::accept(&name, true).await;
    engine.handshake().await;
    let AppToEngine::ApplyTopology { request, topology } = engine.receive_command().await else {
        panic!("expected a topology");
    };
    assert_eq!(topology.revision, TopologyRevision(1));
    engine.send(&EngineToApp::Ack { request }).await;
    assert!(matches!(next_event(&mut events).await, ClientEvent::Connected { .. }));
    assert!(matches!(
        next_event(&mut events).await,
        ClientEvent::TopologyApplied { .. }
    ));

    // 引擎"崩溃"后重新启动：客户端重连，并以更新的修订号重新下发
    drop(engine);
    assert_eq!(
        next_event(&mut events).await,
        ClientEvent::Disconnected(DisconnectReason::Closed)
    );
    let mut engine = FakeEngine::accept(&name, true).await;
    engine.handshake().await;
    let AppToEngine::ApplyTopology { topology, .. } = engine.receive_command().await else {
        panic!("expected the topology again");
    };
    assert_eq!(topology.revision, TopologyRevision(2));
    assert_eq!(topology.nodes[0].id, NodeId(7));
}

#[tokio::test]
async fn a_silent_engine_is_reported_as_unresponsive() {
    let name = pipe_name("silent");
    let (_client, mut events) = start(&name);
    let mut engine = FakeEngine::accept(&name, true).await;
    engine.handshake().await;
    assert!(matches!(next_event(&mut events).await, ClientEvent::Connected { .. }));
    // 不再应答心跳（只读不写）
    let reading = tokio::spawn(async move {
        loop {
            let _ = engine.receive().await;
        }
    });
    assert_eq!(
        next_event(&mut events).await,
        ClientEvent::Disconnected(DisconnectReason::Unresponsive)
    );
    reading.abort();
}

#[tokio::test]
async fn a_protocol_mismatch_is_reported() {
    let name = pipe_name("mismatch");
    let (_client, mut events) = start(&name);
    let mut engine = FakeEngine::accept(&name, true).await;
    let _hello = engine.receive().await;
    let engine_protocol = ProtocolVersion(PROTOCOL_VERSION.0 + 1);
    engine.send(&EngineToApp::HelloRejected { engine_protocol }).await;
    assert_eq!(
        next_event(&mut events).await,
        ClientEvent::Disconnected(DisconnectReason::VersionMismatch { engine_protocol })
    );
}

#[tokio::test]
async fn shutdown_is_confirmed_by_the_engine() {
    let name = pipe_name("shutdown");
    let (client, mut events) = start(&name);
    let mut engine = FakeEngine::accept(&name, true).await;
    engine.handshake().await;
    assert!(matches!(next_event(&mut events).await, ClientEvent::Connected { .. }));
    let server = tokio::spawn(async move {
        let AppToEngine::Shutdown { request } = engine.receive_command().await else {
            panic!("expected shutdown");
        };
        engine.send(&EngineToApp::Ack { request }).await;
        engine
    });
    assert!(client.shutdown_engine().await);
    drop(server.await.unwrap());
}
