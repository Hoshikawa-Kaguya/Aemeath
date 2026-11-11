//! 查询服务（驱动）的运行状态（守护进程检查 usbip-win2 的驱动服务能否运行）。

use std::io;

use windows::Win32::Foundation::ERROR_SERVICE_DOES_NOT_EXIST;
use windows::Win32::System::Services::{
    CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatus, SC_HANDLE, SC_MANAGER_CONNECT,
    SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_STATUS, SERVICE_STOPPED,
};
use windows::core::{HSTRING, PCWSTR};

/// 服务的运行状态，只区分调用方关心的几种。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Running,
    Stopped,
    /// 正在启动、停止、暂停等过渡状态
    Other,
}

/// 服务的当前状态；服务不存在时返回 None。只需要普通用户权限。
pub fn service_state(name: &str) -> io::Result<Option<ServiceState>> {
    // SAFETY: 本机、默认数据库，只请求连接权限；句柄由 Handle 守卫关闭
    let manager = Handle(unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT) }?);
    let name = HSTRING::from(name);
    // SAFETY: manager 有效，name 在调用期间有效；只请求查询状态的权限
    let service = match unsafe { OpenServiceW(manager.0, &name, SERVICE_QUERY_STATUS) } {
        Ok(service) => Handle(service),
        Err(error) if error.code() == ERROR_SERVICE_DOES_NOT_EXIST.to_hresult() => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut status = SERVICE_STATUS::default();
    // SAFETY: service 有效，status 是有效的输出缓冲区
    unsafe { QueryServiceStatus(service.0, &mut status) }?;
    Ok(Some(match status.dwCurrentState {
        SERVICE_RUNNING => ServiceState::Running,
        SERVICE_STOPPED => ServiceState::Stopped,
        _ => ServiceState::Other,
    }))
}

struct Handle(SC_HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: 句柄由 OpenSCManagerW / OpenServiceW 返回，只在这里关闭一次
        let _ = unsafe { CloseServiceHandle(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_core_service_and_a_missing_one() {
        // Windows 音频服务在任何 Windows 11 上都存在
        assert!(service_state("Audiosrv").unwrap().is_some());
        assert_eq!(service_state("AemeathNoSuchService").unwrap(), None);
    }
}
