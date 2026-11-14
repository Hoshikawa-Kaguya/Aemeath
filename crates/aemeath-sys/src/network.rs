//! 本机 TCP 连接属于哪个进程（USB/IP 服务端只接受驱动发起的连接）。

use std::io;
use std::net::SocketAddrV4;

use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, NO_ERROR, WIN32_ERROR};
use windows::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, TCP_TABLE_OWNER_PID_CONNECTIONS,
};
use windows::Win32::Networking::WinSock::AF_INET;

/// System 进程的 PID：内核驱动发起的 TCP 连接归属于它。
pub const SYSTEM_PROCESS_ID: u32 = 4;

/// `MIB_TCPTABLE_OWNER_PID` 按 32 位字读：第一个字是行数，之后每行是 `MIB_TCPROW_OWNER_PID` 的 6 个字
/// （状态、本地地址、本地端口、远端地址、远端端口、PID）。
const ROW_WORDS: usize = size_of::<MIB_TCPROW_OWNER_PID>() / size_of::<u32>();

/// 本地端为 `local`、远端为 `remote` 的那条 IPv4 TCP 连接属于哪个进程；表里没有这条连接时返回 `None`。
pub fn tcp_connection_owner(local: SocketAddrV4, remote: SocketAddrV4) -> io::Result<Option<u32>> {
    let mut size = 0u32;
    // 按 u32 分配，保证表的对齐
    let mut words: Vec<u32> = Vec::new();
    loop {
        // SAFETY: 缓冲可写，`size` 不大于它的字节数；缓冲不够时 API 只写回所需大小
        let status = unsafe {
            GetExtendedTcpTable(
                Some(words.as_mut_ptr().cast()),
                &mut size,
                false,
                u32::from(AF_INET.0),
                TCP_TABLE_OWNER_PID_CONNECTIONS,
                0,
            )
        };
        match WIN32_ERROR(status) {
            NO_ERROR => break,
            // 两次调用之间连接可能变多，按新的大小再试
            ERROR_INSUFFICIENT_BUFFER => words.resize((size as usize).div_ceil(size_of::<u32>()), 0),
            error => return Err(io::Error::from_raw_os_error(error.0 as i32)),
        }
    }
    Ok(find_owner(&words, local, remote))
}

fn find_owner(table: &[u32], local: SocketAddrV4, remote: SocketAddrV4) -> Option<u32> {
    let (&count, rows) = table.split_first()?;
    rows.as_chunks::<ROW_WORDS>().0.iter().take(count as usize).find_map(
        |&[_state, local_address, local_port, remote_address, remote_port, owner]| {
            (endpoint_matches(local_address, local_port, local)
                && endpoint_matches(remote_address, remote_port, remote))
            .then_some(owner)
        },
    )
}

/// 表里的地址和端口都按网络字节序存放（端口在低 16 位），按内存中的字节比较。
fn endpoint_matches(address: u32, port: u32, endpoint: SocketAddrV4) -> bool {
    let [high, low, ..] = port.to_ne_bytes();
    address.to_ne_bytes() == endpoint.ip().octets() && u16::from_be_bytes([high, low]) == endpoint.port()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};

    fn v4(address: SocketAddr) -> SocketAddrV4 {
        match address {
            SocketAddr::V4(address) => address,
            SocketAddr::V6(_) => unreachable!("bound to IPv4"),
        }
    }

    #[test]
    fn finds_the_process_owning_a_loopback_connection() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        let (local, remote) = (v4(client.local_addr().unwrap()), v4(server.local_addr().unwrap()));

        assert_eq!(tcp_connection_owner(local, remote).unwrap(), Some(std::process::id()));
        assert_eq!(tcp_connection_owner(remote, local).unwrap(), Some(std::process::id()));
        let elsewhere = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 1);
        assert_eq!(tcp_connection_owner(local, elsewhere).unwrap(), None);
    }

    #[test]
    fn rows_beyond_the_count_are_ignored() {
        let endpoint = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 32410);
        let address = u32::from_ne_bytes(endpoint.ip().octets());
        let [high, low] = endpoint.port().to_be_bytes();
        let port = u32::from_ne_bytes([high, low, 0, 0]);
        let row = [0, address, port, address, port, 42];
        let mut table = vec![1];
        table.extend(row);
        assert_eq!(find_owner(&table, endpoint, endpoint), Some(42));
        table[0] = 0;
        assert_eq!(find_owner(&table, endpoint, endpoint), None);
    }
}
