//! USB/IP 服务端口由守护进程决定，再分发给引擎和主程序（主程序用它调用 `usbip.exe`）。
//!
//! Windows 可能把任意固定端口划进保留段（实测重启后旧的默认端口被 winnat 占走，绑定报 10013），
//! 正式版不能依赖固定端口：每次启动引擎前探测，不可用就让系统分配。开发版用固定端口，方便调试和对照日志。

use std::io;
use std::net::{Ipv4Addr, TcpListener};

use aemeath_usbip::identity::{DEFAULT_USBIP_PORT, IdentityProfile};
use tracing::warn;

/// 开发版固定使用的端口，被保留时改这里。
/// 当前保留段可用 `netsh interface ipv4 show excludedportrange protocol=tcp` 查看。
const DEV_USBIP_PORT: u16 = DEFAULT_USBIP_PORT;

/// 固定端口：命令行明确指定的优先，其次是开发版的配置；正式版返回 None，由 [`resolve`] 探测。
pub fn fixed(explicit: Option<u16>, identity: IdentityProfile) -> Option<u16> {
    explicit.or(match identity {
        IdentityProfile::Development => Some(DEV_USBIP_PORT),
        IdentityProfile::Production => None,
    })
}

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

    #[test]
    fn only_the_production_build_probes() {
        assert_eq!(fixed(None, IdentityProfile::Development), Some(DEV_USBIP_PORT));
        assert_eq!(fixed(None, IdentityProfile::Production), None);
        assert_eq!(fixed(Some(4000), IdentityProfile::Production), Some(4000));
        assert_eq!(fixed(Some(4000), IdentityProfile::Development), Some(4000));
    }
}
