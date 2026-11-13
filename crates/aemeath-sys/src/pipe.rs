//! 只允许当前用户访问的命名管道（IPC 的访问控制只允许当前用户）。
//!
//! - 安全描述符只给当前用户授权（受保护的 DACL，不继承），其他用户（包括管理员）都无法连接。
//! - 拒绝远程客户端。
//! - 可要求"首个实例"：同名管道已被别的进程创建时失败，防止被抢先占用。

use std::ffi::c_void;
use std::io;
use std::os::windows::io::AsRawHandle;

use tokio::net::windows::named_pipe::{NamedPipeClient, NamedPipeServer, PipeMode, ServerOptions};
use windows::Win32::Foundation::{HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{GetTokenInformation, TOKEN_USER, TokenUser};
use windows::Win32::System::Pipes::GetNamedPipeServerProcessId;
use windows::core::PWSTR;

use crate::security::CurrentUserOnly;
use crate::token::Token;

/// 当前用户的 SID，例如 `S-1-5-21-…`。
pub fn current_user_sid() -> io::Result<String> {
    user_sid(&Token::current_process()?)
}

/// 已连上的管道的服务端进程属于哪个用户（SID）。管道名是全机共享的，同机另一个用户可以抢先建同名管道，
/// 客户端连上后用它核对；打不开对方进程（通常就是别的用户的进程）时返回错误。
pub fn pipe_server_user_sid(pipe: &NamedPipeClient) -> io::Result<String> {
    let mut process_id = 0u32;
    // SAFETY: 句柄来自仍然打开的管道客户端；process_id 是有效的输出指针
    unsafe { GetNamedPipeServerProcessId(HANDLE(pipe.as_raw_handle()), &mut process_id) }?;
    user_sid(&Token::of_process(process_id)?)
}

fn user_sid(token: &Token) -> io::Result<String> {
    let mut needed = 0u32;
    // SAFETY: 第一次调用只查询所需长度（不传缓冲区），预期返回"缓冲区不足"错误，忽略之。
    let _ = unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &mut needed) };
    // 用 u64 数组保证 TOKEN_USER 所需的对齐
    let mut buffer = vec![0u64; (needed as usize).div_ceil(std::mem::size_of::<u64>())];
    // SAFETY: 缓冲区至少 needed 字节且按 8 字节对齐，句柄有效。
    unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buffer.as_mut_ptr().cast::<c_void>()),
            needed,
            &mut needed,
        )
    }?;
    // SAFETY: 调用成功后缓冲区开头是一个 TOKEN_USER，其中的 SID 指针指向同一缓冲区内部，buffer 在此作用域内一直存活。
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };

    let mut text = PWSTR::null();
    // SAFETY: SID 指针来自上面的 TOKEN_USER；成功后 text 指向系统分配的字符串，由 LocalString 守卫释放。
    unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) }?;
    let text = LocalString(text);
    // SAFETY: text 是系统返回的以 NUL 结尾的 UTF-16 字符串，在守卫释放前有效。
    unsafe { text.0.to_string() }.map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// 创建一个只允许当前用户连接的字节流命名管道服务端实例。
///
/// `first_instance` 为 true 时，同名管道已存在（被别的进程或另一个引擎实例占用）就失败。
pub fn create_pipe_server(name: &str, first_instance: bool) -> io::Result<NamedPipeServer> {
    let mut security = CurrentUserOnly::new()?;

    let mut options = ServerOptions::new();
    options
        .pipe_mode(PipeMode::Byte)
        .reject_remote_clients(true)
        .first_pipe_instance(first_instance);
    // SAFETY: 安全属性完整初始化，其中的描述符在调用期间由 security 保持有效；
    // CreateNamedPipeW 只在调用时读取它们，不保留指针。
    unsafe { options.create_with_security_attributes_raw(name, security.attributes().cast::<c_void>()) }
}

/// 由系统用 LocalAlloc 分配的字符串，析构时释放。
struct LocalString(PWSTR);

impl Drop for LocalString {
    fn drop(&mut self) {
        // SAFETY: 指针由 ConvertSidToStringSidW 分配，只在这里释放一次。
        let _ = unsafe { LocalFree(Some(HLOCAL(self.0.0.cast()))) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_current_user_sid() {
        let sid = current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-5-"), "{sid}");
    }

    #[tokio::test]
    async fn the_server_of_our_own_pipe_is_the_current_user() {
        let name = format!(r"\\.\pipe\aemeath-sys-owner-test-{}", std::process::id());
        let _server = create_pipe_server(&name, true).unwrap();
        let client = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(&name)
            .unwrap();
        assert_eq!(pipe_server_user_sid(&client).unwrap(), current_user_sid().unwrap());
    }

    #[tokio::test]
    async fn first_instance_prevents_a_second_server_with_the_same_name() {
        let name = format!(r"\\.\pipe\aemeath-sys-test-{}", std::process::id());
        let _first = create_pipe_server(&name, true).unwrap();
        assert!(create_pipe_server(&name, true).is_err());
    }
}
