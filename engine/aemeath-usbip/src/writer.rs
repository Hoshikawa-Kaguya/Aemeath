//! 连接的写端：调度线程（实时）把应答放进有界队列，由每条连接一个写线程写到 socket。
//!
//! 写 socket 会阻塞：主机暂时不读、发送缓冲满了，调用方就停住。调度线程负责全部槽位和混音，不能等任何一条连接。
//! 写线程还负责释放内存，并把写完的缓冲还给调度线程复用，这样逐 URB 的路径不用申请和释放内存。

use std::io::{self, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread;
use std::time::Duration;

use aemeath_sys::thread_priority::raise_current_thread;

use crate::wire::Submit;

/// 队列长度：Windows 实测每个 URB 10 ms，约 2.5 s 的应答
const QUEUE_CAPACITY: usize = 256;
/// 主机这么久不读，就放弃这条连接（由挂载维护重新连接）
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug)]
struct Outgoing {
    bytes: Vec<u8>,
    /// 这个应答对应的 URB：读线程申请的内存，交给写线程释放
    submit: Option<Submit>,
}

/// 调度线程一侧的句柄。丢弃后写线程写完队列里的内容就退出。
#[derive(Debug)]
pub struct ConnectionWriter {
    queue: SyncSender<Outgoing>,
    recycled: Receiver<Vec<u8>>,
    socket: TcpStream,
    abandoned: Arc<AtomicBool>,
}

impl ConnectionWriter {
    /// 启动写线程。返回的标志变为 true 时，这条连接已被放弃，读线程应退出。
    pub fn spawn(stream: TcpStream, connection: u64) -> io::Result<(Self, Arc<AtomicBool>)> {
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        let socket = stream.try_clone()?;
        let (queue, pending) = sync_channel(QUEUE_CAPACITY);
        let (give_back, recycled) = sync_channel(QUEUE_CAPACITY);
        let abandoned = Arc::new(AtomicBool::new(false));
        let thread_abandoned = Arc::clone(&abandoned);
        thread::Builder::new()
            .name(format!("usbip-write-{connection}"))
            .spawn(move || run(stream, &pending, &give_back, &thread_abandoned))?;
        let writer = Self {
            queue,
            recycled,
            socket,
            abandoned: Arc::clone(&abandoned),
        };
        Ok((writer, abandoned))
    }

    /// 取一个空缓冲来编码应答（优先复用写完的）。不阻塞。
    pub fn buffer(&self) -> Vec<u8> {
        self.recycled.try_recv().unwrap_or_default()
    }

    /// 把应答放进队列，不阻塞。队列满或写线程已退出时返回错误，这条连接随之作废。
    pub fn send(&self, bytes: Vec<u8>, submit: Option<Submit>) -> io::Result<()> {
        match self.queue.try_send(Outgoing { bytes, submit }) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                self.abandoned.store(true, Ordering::Relaxed);
                let _ = self.socket.shutdown(Shutdown::Both);
                Err(io::ErrorKind::WouldBlock.into())
            }
            Err(TrySendError::Disconnected(_)) => Err(io::ErrorKind::BrokenPipe.into()),
        }
    }
}

fn run(mut stream: TcpStream, pending: &Receiver<Outgoing>, give_back: &SyncSender<Vec<u8>>, abandoned: &AtomicBool) {
    if let Err(error) = raise_current_thread() {
        tracing::warn!(%error, "cannot raise the USB/IP writer thread priority");
    }
    while let Ok(Outgoing { mut bytes, submit }) = pending.recv() {
        if let Err(error) = stream.write_all(&bytes) {
            tracing::info!(%error, "USB/IP write failed, abandoning the connection");
            abandoned.store(true, Ordering::Relaxed);
            let _ = stream.shutdown(Shutdown::Both);
            return;
        }
        drop(submit);
        bytes.clear();
        let _ = give_back.try_send(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;
    use std::time::Instant;

    fn connected() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        (client, listener.accept().unwrap().0)
    }

    #[test]
    fn delivers_in_order_and_recycles_buffers() {
        let (ours, mut peer) = connected();
        let (writer, abandoned) = ConnectionWriter::spawn(ours, 1).unwrap();
        writer.send(b"ab".to_vec(), None).unwrap();
        writer.send(b"cd".to_vec(), None).unwrap();
        let mut received = [0u8; 4];
        peer.read_exact(&mut received).unwrap();
        assert_eq!(&received, b"abcd");
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut reused = writer.buffer();
        while reused.capacity() == 0 && Instant::now() < deadline {
            thread::yield_now();
            reused = writer.buffer();
        }
        assert!(reused.capacity() > 0 && reused.is_empty());
        assert!(!abandoned.load(Ordering::Relaxed));
    }

    #[test]
    fn a_peer_that_stops_reading_never_blocks_the_sender() {
        const LARGE: usize = 1 << 20;
        let (ours, _peer) = connected();
        let (writer, abandoned) = ConnectionWriter::spawn(ours, 2).unwrap();
        let started = Instant::now();
        let error = (0..=QUEUE_CAPACITY + 2)
            .find_map(|_| writer.send(vec![0; LARGE], None).err())
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert!(abandoned.load(Ordering::Relaxed));
        assert!(
            started.elapsed() < WRITE_TIMEOUT,
            "sending must not wait for the socket"
        );
    }
}
