//! USB/IP 服务端：每个被导入的槽位一条 TCP 连接，只监听 127.0.0.1。
//!
//! 线程模型（与 S4 验证过的结构一致）：
//! - 一个监听线程，加上每条连接一个读线程（非实时）。读线程解析 URB，在本地处理控制请求，
//!   并把一切需要写回连接的内容作为 [`ServerEvent`] 交给引擎。
//! - 导入成功后，连接的写端随 [`ServerEvent::Imported`] 交给引擎的调度线程。此后**只有调度线程**
//!   向这条连接写数据，所以控制请求的应答也经事件转交，不在读线程里直接写。
//!
//! 只有被导出的槽位（路由拓扑里被占用的槽位）可以导入；重复的导入返回"设备忙"（ADR-0007）。

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use aemeath_protocol::ids::{SlotDirection, SlotId};
use aemeath_sys::clock::now_hns;

use crate::descriptors::Descriptors;
use crate::function::{Setup, StreamChange, Uac2Function};
use crate::identity::{IdentityProfile, SlotIdentity};
use crate::wire::{
    self, BUSID_LEN, BusId, DeviceRecord, Direction, OP_HEADER_LEN, OpRequest, OpStatus, RetSubmit, Submit, UrbCommand,
    UrbDecoder, UrbStatus, WireError,
};

/// usbip-win2 默认连接的端口
pub const DEFAULT_PORT: u16 = 3241;
/// 同时存在的连接数上限：16 个槽位，加上残留的后台重连和 `usbip.exe` 的查询，留足余量
const MAX_CONNECTIONS: usize = 64;
/// 连接建立后必须在这段时间内发来操作请求，否则断开，防止空连接一直占着名额
const OP_PHASE_TIMEOUT: Duration = Duration::from_secs(5);
/// 导入后读线程至少每隔这么久检查一次槽位是否仍被导出。
/// Windows 上 `shutdown` 不保证唤醒另一个句柄上阻塞中的 `recv`，没有音频流量的连接可能一直不退出。
const EXPORT_CHECK_INTERVAL: Duration = Duration::from_secs(1);
const READ_CHUNK: usize = 64 * 1024;
/// 输入槽位（主机播放）的等时 OUT 端点号与输出槽位（主机录音）的等时 IN 端点号
const RENDER_ENDPOINT_NUMBER: u8 = 1;
const CAPTURE_ENDPOINT_NUMBER: u8 = 2;
const CONTROL_ENDPOINT: u8 = 0;

/// 一条连接的编号，用于把事件对应到连接上。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectionId(pub u64);

/// 服务端交给引擎的事件。同一条连接的事件按发生顺序送达。
#[derive(Debug)]
pub enum ServerEvent {
    /// 主机导入了一个槽位。`writer` 是连接的写端，交给调度线程独占使用。
    Imported {
        slot: SlotId,
        connection: ConnectionId,
        writer: TcpStream,
    },
    /// 连接已断开（主机卸载、出错，或槽位不再被导出）。
    Released { slot: SlotId, connection: ConnectionId },
    /// 需要尽快写回的应答（控制请求、不支持的传输）。
    Reply { connection: ConnectionId, bytes: Vec<u8> },
    /// 流开始、停止或格式改变。先于之后的等时 URB 送达。
    StreamChanged {
        slot: SlotId,
        connection: ConnectionId,
        change: StreamChange,
    },
    /// 一个等时 URB，由调度线程按时间线完成。
    IsoSubmit {
        slot: SlotId,
        connection: ConnectionId,
        submit: Submit,
        /// 读线程解析出这个 URB 的时刻（QPC，100 ns），用于诊断从读线程到调度线程的排队延迟
        received_hns: i64,
    },
    /// 主机取消一个尚未完成的 URB。
    Unlink {
        connection: ConnectionId,
        seq: u32,
        target_seq: u32,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct ServerConfig {
    /// 监听端口；测试中用 0 让系统分配
    pub port: u16,
    pub profile: IdentityProfile,
}

pub struct UsbipServer {
    local_addr: SocketAddr,
    shared: Arc<Shared>,
}

struct Shared {
    identities: BTreeMap<SlotId, SlotIdentity>,
    state: Mutex<State>,
    events: Sender<ServerEvent>,
    connections: AtomicUsize,
    next_connection: AtomicU64,
}

#[derive(Default)]
struct State {
    exported: BTreeSet<SlotId>,
    /// 已导入的槽位，以及用来在取消导出时强制断开的连接句柄
    imported: HashMap<SlotId, (ConnectionId, TcpStream)>,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        // 持锁期间只做集合操作，不会在中途 panic；即使出现中毒，数据仍然一致
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl UsbipServer {
    /// 开始监听。初始时没有任何槽位被导出，所有导入都会被拒绝，直到调用 [`Self::set_exported`]。
    pub fn start(config: ServerConfig, events: Sender<ServerEvent>) -> io::Result<Self> {
        let identities = config
            .profile
            .all()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?
            .into_iter()
            .map(|identity| (identity.slot, identity))
            .collect();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, config.port))?;
        let local_addr = listener.local_addr()?;
        let shared = Arc::new(Shared {
            identities,
            state: Mutex::new(State::default()),
            events,
            connections: AtomicUsize::new(0),
            next_connection: AtomicU64::new(1),
        });
        let accept_shared = Arc::clone(&shared);
        thread::Builder::new()
            .name("usbip-listen".to_owned())
            .spawn(move || accept_loop(&listener, &accept_shared))?;
        tracing::info!(%local_addr, "USB/IP server listening");
        Ok(Self { local_addr, shared })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// 设定被导出的槽位（即路由拓扑中被占用的槽位）。不再导出的槽位如果正被导入，立即断开它的连接。
    pub fn set_exported(&self, slots: BTreeSet<SlotId>) {
        let mut state = self.shared.state();
        for (slot, (_, stream)) in &state.imported {
            if !slots.contains(slot) {
                tracing::info!(%slot, "slot no longer exported, closing its connection");
                // 读线程随后读到错误，发出 Released 并清理状态
                let _ = stream.shutdown(Shutdown::Both);
            }
        }
        state.exported = slots;
    }
}

fn accept_loop(listener: &TcpListener, shared: &Arc<Shared>) {
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(error) => {
                tracing::warn!(%error, "accept failed");
                continue;
            }
        };
        if shared.connections.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
            shared.connections.fetch_sub(1, Ordering::AcqRel);
            tracing::warn!("too many connections, refusing one");
            continue;
        }
        let connection = ConnectionId(shared.next_connection.fetch_add(1, Ordering::Relaxed));
        let connection_shared = Arc::clone(shared);
        let spawned = thread::Builder::new()
            .name(format!("usbip-conn-{}", connection.0))
            .spawn(move || {
                if let Err(error) = serve(stream, connection, &connection_shared) {
                    tracing::debug!(connection = connection.0, %error, "connection ended");
                }
                connection_shared.connections.fetch_sub(1, Ordering::AcqRel);
            });
        if let Err(error) = spawned {
            shared.connections.fetch_sub(1, Ordering::AcqRel);
            tracing::warn!(%error, "cannot start a connection thread");
        }
    }
}

fn serve(mut stream: TcpStream, connection: ConnectionId, shared: &Shared) -> io::Result<()> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(OP_PHASE_TIMEOUT))?;
    let mut header = [0u8; OP_HEADER_LEN];
    stream.read_exact(&mut header)?;
    match wire::parse_op_header(&header).map_err(invalid_data)? {
        OpRequest::DevList => {
            let records: Vec<DeviceRecord> = {
                let state = shared.state();
                state
                    .exported
                    .iter()
                    .filter_map(|slot| shared.identities.get(slot))
                    .map(device_record)
                    .collect()
            };
            stream.write_all(&wire::encode_devlist_reply(&records))
        }
        OpRequest::Import => {
            let mut raw = [0u8; BUSID_LEN];
            stream.read_exact(&mut raw)?;
            let Some(identity) = admit(&stream, &raw, connection, shared)? else {
                return Ok(());
            };
            let slot = identity.slot;
            let result = import(stream, identity, connection, shared);
            {
                let mut state = shared.state();
                if state.imported.get(&slot).is_some_and(|(owner, _)| *owner == connection) {
                    state.imported.remove(&slot);
                }
            }
            let _ = shared.events.send(ServerEvent::Released { slot, connection });
            tracing::info!(%slot, connection = connection.0, "slot released");
            result
        }
    }
}

/// 检查导入请求；接受时登记为已导入并回复设备信息，拒绝时回复原因并返回 None。
fn admit(
    stream: &TcpStream,
    raw_busid: &[u8; BUSID_LEN],
    connection: ConnectionId,
    shared: &Shared,
) -> io::Result<Option<SlotIdentity>> {
    let busid = BusId::parse(raw_busid).ok();
    let identity = busid
        .as_ref()
        .and_then(|busid| shared.identities.values().find(|identity| identity.busid == *busid));
    let decision = {
        let mut state = shared.state();
        match identity {
            Some(identity) if !state.exported.contains(&identity.slot) => Err(OpStatus::NoDevice),
            Some(identity) if state.imported.contains_key(&identity.slot) => Err(OpStatus::DeviceBusy),
            Some(identity) => {
                state.imported.insert(identity.slot, (connection, stream.try_clone()?));
                Ok(identity.clone())
            }
            None => Err(OpStatus::NoDevice),
        }
    };
    let mut writer = stream;
    match decision {
        Ok(identity) => {
            writer.write_all(&wire::encode_import_reply(
                OpStatus::Ok,
                Some(&device_record(&identity)),
            ))?;
            tracing::info!(slot = %identity.slot, connection = connection.0, "slot imported");
            Ok(Some(identity))
        }
        Err(status) => {
            writer.write_all(&wire::encode_import_reply(status, None))?;
            // 残留的后台重连会反复出现，只记 debug，避免刷屏
            tracing::debug!(busid = ?busid.map(|busid| busid.to_string()), ?status, "import refused");
            Ok(None)
        }
    }
}

fn import(mut stream: TcpStream, identity: SlotIdentity, connection: ConnectionId, shared: &Shared) -> io::Result<()> {
    stream.set_read_timeout(Some(EXPORT_CHECK_INTERVAL))?;
    let slot = identity.slot;
    let writer = stream.try_clone()?;
    send(
        shared,
        ServerEvent::Imported {
            slot,
            connection,
            writer,
        },
    )?;

    let mut function = Uac2Function::new(slot.direction, Descriptors::build(&identity));
    let mut decoder = UrbDecoder::new();
    let mut chunk = vec![0u8; READ_CHUNK];
    loop {
        let read = match stream.read(&mut chunk) {
            Ok(0) => return Ok(()),
            Ok(read) => read,
            Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                if shared.state().exported.contains(&slot) {
                    continue;
                }
                tracing::debug!(%slot, "slot no longer exported, ending the idle connection");
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        decoder.push(&chunk[..read]);
        while let Some(command) = decoder.next_command().map_err(invalid_data)? {
            handle_command(command, slot, connection, &mut function, shared)?;
        }
    }
}

fn handle_command(
    command: UrbCommand,
    slot: SlotId,
    connection: ConnectionId,
    function: &mut Uac2Function,
    shared: &Shared,
) -> io::Result<()> {
    let submit = match command {
        UrbCommand::Unlink { seq, target_seq } => {
            return send(
                shared,
                ServerEvent::Unlink {
                    connection,
                    seq,
                    target_seq,
                },
            );
        }
        UrbCommand::Submit(submit) => submit,
    };

    if submit.endpoint == CONTROL_ENDPOINT {
        let outcome = function.control(&Setup::parse(&submit.setup), &submit.out_data);
        if let Some(change) = outcome.stream {
            send(
                shared,
                ServerEvent::StreamChanged {
                    slot,
                    connection,
                    change,
                },
            )?;
        }
        let actual_length = match submit.direction {
            Direction::Out => submit.out_data.len(),
            Direction::In => outcome.data.len(),
        };
        let bytes = wire::encode_ret_submit(&RetSubmit {
            seq: submit.seq,
            status: outcome.status,
            actual_length: u32::try_from(actual_length).unwrap_or(u32::MAX),
            start_frame: 0,
            iso_packets: None,
            error_count: 0,
            data: &outcome.data,
        });
        return send(shared, ServerEvent::Reply { connection, bytes });
    }

    let expected = match function.direction() {
        SlotDirection::In => (RENDER_ENDPOINT_NUMBER, Direction::Out),
        SlotDirection::Out => (CAPTURE_ENDPOINT_NUMBER, Direction::In),
    };
    if submit.is_iso() && (submit.endpoint, submit.direction) == expected {
        return send(
            shared,
            ServerEvent::IsoSubmit {
                slot,
                connection,
                submit,
                received_hns: now_hns(),
            },
        );
    }

    tracing::debug!(%slot, endpoint = submit.endpoint, "unexpected transfer, stalling");
    let bytes = wire::encode_ret_submit(&RetSubmit {
        seq: submit.seq,
        status: UrbStatus::Stall,
        actual_length: 0,
        start_frame: 0,
        iso_packets: None,
        error_count: 0,
        data: &[],
    });
    send(shared, ServerEvent::Reply { connection, bytes })
}

fn device_record(identity: &SlotIdentity) -> DeviceRecord {
    let descriptors = Descriptors::build(identity);
    DeviceRecord {
        busid: identity.busid.clone(),
        device_number: identity.device_number,
        vendor_id: identity.vendor_id,
        product_id: identity.product_id,
        device_release: identity.device_release,
        class: descriptors.device_class,
        interfaces: descriptors.interface_classes,
    }
}

/// 引擎已经不再接收事件时结束连接。
fn send(shared: &Shared, event: ServerEvent) -> io::Result<()> {
    shared
        .events
        .send(event)
        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "engine stopped receiving server events"))
}

fn invalid_data(error: WireError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aemeath_protocol::ids::SlotNumber;
    use std::sync::mpsc::{self, Receiver};

    const EVENT_TIMEOUT: Duration = Duration::from_secs(5);

    fn slot(direction: SlotDirection, number: u8) -> SlotId {
        SlotId {
            direction,
            number: SlotNumber::new(number).unwrap(),
        }
    }

    fn start() -> (UsbipServer, Receiver<ServerEvent>) {
        let (events, receiver) = mpsc::channel();
        let config = ServerConfig {
            port: 0,
            profile: IdentityProfile::Development,
        };
        (UsbipServer::start(config, events).unwrap(), receiver)
    }

    /// 发出导入请求，返回 (状态, 连接)
    fn request_import(server: &UsbipServer, busid: &str) -> (u32, TcpStream) {
        let mut stream = TcpStream::connect(server.local_addr()).unwrap();
        let mut request = vec![0x01, 0x11, 0x80, 0x03, 0, 0, 0, 0];
        let mut raw = [0u8; BUSID_LEN];
        raw[..busid.len()].copy_from_slice(busid.as_bytes());
        request.extend_from_slice(&raw);
        stream.write_all(&request).unwrap();
        let mut reply = [0u8; 8];
        stream.read_exact(&mut reply).unwrap();
        let status = u32::from_be_bytes([reply[4], reply[5], reply[6], reply[7]]);
        if status == 0 {
            let mut record = [0u8; 312];
            stream.read_exact(&mut record).unwrap();
        }
        (status, stream)
    }

    fn next_event(receiver: &Receiver<ServerEvent>) -> ServerEvent {
        receiver.recv_timeout(EVENT_TIMEOUT).unwrap()
    }

    #[test]
    fn only_exported_slots_can_be_imported_once() {
        let (server, events) = start();
        let (status, _refused) = request_import(&server, "1-1");
        assert_eq!(status, 4, "nothing exported yet");

        server.set_exported(BTreeSet::from([slot(SlotDirection::In, 1)]));
        let (status, _first) = request_import(&server, "1-1");
        assert_eq!(status, 0);
        let ServerEvent::Imported { slot: imported, .. } = next_event(&events) else {
            unreachable!("expected Imported")
        };
        assert_eq!(imported, slot(SlotDirection::In, 1));

        let (status, _second) = request_import(&server, "1-1");
        assert_eq!(status, 2, "duplicate import is reported as busy");
        let (status, _unknown) = request_import(&server, "9-9");
        assert_eq!(status, 4);
    }

    #[test]
    fn control_requests_are_answered_through_events() {
        let (server, events) = start();
        server.set_exported(BTreeSet::from([slot(SlotDirection::Out, 2)]));
        let (status, mut stream) = request_import(&server, "1-10");
        assert_eq!(status, 0);
        assert!(matches!(next_event(&events), ServerEvent::Imported { .. }));

        // GET_DESCRIPTOR(device)，ep0，IN 方向
        let mut submit = Vec::new();
        for value in [1u32, 7, 0, 1, 0, 0, 64, 0, 0xffff_ffff, 0] {
            submit.extend_from_slice(&value.to_be_bytes());
        }
        submit.extend_from_slice(&[0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 0x40, 0x00]);
        stream.write_all(&submit).unwrap();

        let ServerEvent::Reply { bytes, .. } = next_event(&events) else {
            unreachable!("expected Reply")
        };
        // 48 字节应答头 + 18 字节设备描述符
        assert_eq!(bytes.len(), 48 + 18);
        assert_eq!(&bytes[4..8], &7u32.to_be_bytes());
    }

    #[test]
    fn unexporting_a_slot_closes_its_connection() {
        let (server, events) = start();
        let exported = slot(SlotDirection::In, 3);
        server.set_exported(BTreeSet::from([exported]));
        let (status, _stream) = request_import(&server, "1-3");
        assert_eq!(status, 0);
        assert!(matches!(next_event(&events), ServerEvent::Imported { .. }));

        server.set_exported(BTreeSet::new());
        let ServerEvent::Released { slot: released, .. } = next_event(&events) else {
            unreachable!("expected Released")
        };
        assert_eq!(released, exported);

        server.set_exported(BTreeSet::from([exported]));
        let (status, _again) = request_import(&server, "1-3");
        assert_eq!(status, 0, "the slot can be imported again after release");
    }

    #[test]
    fn malformed_urbs_end_the_connection() {
        let (server, events) = start();
        server.set_exported(BTreeSet::from([slot(SlotDirection::In, 4)]));
        let (_, mut stream) = request_import(&server, "1-4");
        assert!(matches!(next_event(&events), ServerEvent::Imported { .. }));

        stream.write_all(&[0xFF; 48]).unwrap();
        assert!(matches!(next_event(&events), ServerEvent::Released { .. }));
    }
}
