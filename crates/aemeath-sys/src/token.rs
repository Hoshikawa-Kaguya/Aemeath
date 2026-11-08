//! 当前进程的访问令牌（查询用户 SID、是否已提权）。

use std::io;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::TOKEN_QUERY;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

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
}

impl Drop for Token {
    fn drop(&mut self) {
        // SAFETY: 句柄由 OpenProcessToken 返回，只在这里关闭一次
        let _ = unsafe { CloseHandle(self.0) };
    }
}
