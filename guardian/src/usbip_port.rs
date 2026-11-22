//! USB/IP 服务端口由守护进程决定，再分发给引擎和主程序（主程序在握手时也会以引擎实际端口为准）。
//!
//! Windows 可能把任意固定端口划进保留段（实测重启后旧的默认端口被 winnat 占走，绑定报 10013），
//! 未明确指定端口时每次启动引擎前探测默认端口，不可用就让系统分配；可用时保持原端口，方便调试和对照日志。

use std::io;
use std::net::{Ipv4Addr, TcpListener};

use tracing::warn;

/// `current` 还能绑定就继续用，否则由系统分配一个当时可用的端口。
///
/// 探测后立即释放，到引擎绑定之间端口仍可能被抢走：那样引擎启动失败，守护进程重启它时会再探测一次。
pub fn resolve(current: u16) -> io::Result<u16> {
    if TcpListener::bind((Ipv4Addr::LOCALHOST, current)).is_ok() {
        return Ok(current);
    }
    let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?.local_addr()?.port();
    warn!(
        unavailable = current,
        port, "the USB/IP port is not available, using another one"
    );
    Ok(port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_free_port_is_kept_and_a_taken_one_is_replaced() {
        let taken = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = taken.local_addr().unwrap().port();
        let replacement = resolve(port).unwrap();
        assert_ne!(replacement, port);
        assert_ne!(replacement, 0);
        drop(taken);
        assert_eq!(resolve(port).unwrap(), port);
    }
}
