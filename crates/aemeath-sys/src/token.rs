//! 当前进程的访问令牌（查询用户 SID、是否已提权）。

use std::io;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::TOKEN_QUERY;
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// 进程令牌句柄，析构时关闭。
pub(crate) struct Token(pub(crate) HANDLE);

impl Token {
    /// 打开当前进程的令牌（只读查询）。
    pub(crate) fn current_process() -> io::Result<Self> {
        let mut token = HANDLE::default();
        // SAFETY: GetCurrentProcess 返回伪句柄，无需关闭；token 是有效的输出指针，成功后由守卫关闭
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }?;
        Ok(Self(token))
    }

    /// 打开另一个进程的令牌（只读查询）。别的用户的进程通常打不开，返回错误。
    pub(crate) fn of_process(process_id: u32) -> io::Result<Self> {
        // SAFETY: 只请求查询权限；成功后的进程句柄在下面关闭
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id) }?;
        let mut token = HANDLE::default();
        // SAFETY: process 是上面打开的有效句柄；token 是有效的输出指针，成功后由守卫关闭
        let opened = unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) };
        // SAFETY: process 只在这里关闭一次
        let _ = unsafe { CloseHandle(process) };
        opened?;
        Ok(Self(token))
    }
}

impl Drop for Token {
    fn drop(&mut self) {
        // SAFETY: 句柄由 OpenProcessToken 返回，只在这里关闭一次
        let _ = unsafe { CloseHandle(self.0) };
    }
}
