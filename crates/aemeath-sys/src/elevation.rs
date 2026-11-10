//! 提权：当前进程是否以管理员身份运行（卸载设备实例需要，ADR-0009），以及用 UAC 提权启动另一个进程（ADR-0011）。

use std::ffi::c_void;
use std::io;
use std::path::Path;

use thiserror::Error;
use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TokenElevation};
use windows::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, w};

use crate::token::Token;

#[derive(Debug, Error)]
pub enum ElevateError {
    /// 用户在 UAC 提示里选了"否"
    #[error("the elevation prompt was declined")]
    Cancelled,
    #[error("cannot start the elevated process: {0}")]
    Failed(#[source] io::Error),
}

/// 提权启动的进程。析构时只关闭句柄，不影响进程本身。
#[derive(Debug)]
pub struct ElevatedProcess(HANDLE);

impl ElevatedProcess {
    /// 等进程退出，返回退出码。
    pub fn wait(self) -> io::Result<u32> {
        // SAFETY: 句柄由 ShellExecuteExW 返回，在 self 析构之前一直有效
        if unsafe { WaitForSingleObject(self.0, INFINITE) } != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }
        let mut code = 0u32;
        // SAFETY: 句柄同上，有 PROCESS_QUERY_INFORMATION 权限；输出是一个有效的 u32
        unsafe { GetExitCodeProcess(self.0, &mut code) }?;
        Ok(code)
    }
}

impl Drop for ElevatedProcess {
    fn drop(&mut self) {
        // SAFETY: 句柄由本结构独占，只在这里关闭一次
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// 用 UAC 提权启动程序。`arguments` 是完整的命令行参数（调用方负责加引号）。
pub fn run_elevated(program: &Path, arguments: &str) -> Result<ElevatedProcess, ElevateError> {
    let file = HSTRING::from(program.as_os_str());
    let parameters = HSTRING::from(arguments);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: info 完整初始化，其中的字符串由上面的 HSTRING 持有，在调用返回前一直有效
    match unsafe { ShellExecuteExW(&mut info) } {
        Ok(()) => Ok(ElevatedProcess(info.hProcess)),
        Err(error) if error.code() == ERROR_CANCELLED.to_hresult() => Err(ElevateError::Cancelled),
        Err(error) => Err(ElevateError::Failed(error.into())),
    }
}

pub fn is_elevated() -> io::Result<bool> {
    let token = Token::current_process()?;
    let mut elevation = TOKEN_ELEVATION::default();
    let mut returned = 0u32;
    // SAFETY: 句柄有效；输出缓冲区是一个完整的 TOKEN_ELEVATION，大小如实传入
    unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            Some((&raw mut elevation).cast::<c_void>()),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
    }?;
    Ok(elevation.TokenIsElevated != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn can_tell_whether_the_process_is_elevated() {
        // 结果取决于测试的运行方式，这里只要求能查询
        assert!(is_elevated().is_ok());
    }
}
