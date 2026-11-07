//! 只允许当前用户访问的命名管道（ADR-0008：IPC 的访问控制只允许当前用户）。
//!
//! - 安全描述符只给当前用户授权（受保护的 DACL，不继承），其他用户（包括管理员）都无法连接。
//! - 拒绝远程客户端。
//! - 可要求"首个实例"：同名管道已被别的进程创建时失败，防止被抢先占用。

use std::ffi::c_void;
use std::io;

use tokio::net::windows::named_pipe::{NamedPipeServer, PipeMode, ServerOptions};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{HSTRING, PWSTR};

/// 当前用户的 SID，例如 `S-1-5-21-…`。
pub fn current_user_sid() -> io::Result<String> {
    let mut token = HANDLE::default();
    // SAFETY: GetCurrentProcess 返回伪句柄，无需关闭；token 是有效的输出指针，成功后由下面的 Token 守卫关闭。
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }?;
    let token = Token(token);

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
    let sid = current_user_sid()?;
    // 受保护的 DACL（P），只有一条：允许当前用户完全访问（GA）
    let sddl = HSTRING::from(format!("D:P(A;;GA;;;{sid})"));
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: sddl 是有效的宽字符串；成功后 descriptor 指向系统分配的描述符，由 LocalDescriptor 守卫释放。
    unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(&sddl, SDDL_REVISION_1, &mut descriptor, None) }?;
    let descriptor = LocalDescriptor(descriptor);
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0.0,
        bInheritHandle: false.into(),
    };

    let mut options = ServerOptions::new();
    options
        .pipe_mode(PipeMode::Byte)
        .reject_remote_clients(true)
        .first_pipe_instance(first_instance);
    // SAFETY: attributes 是完整初始化的 SECURITY_ATTRIBUTES，其中的描述符在调用期间由守卫保持有效；
    // CreateNamedPipeW 只在调用时读取它们，不保留指针。
    unsafe { options.create_with_security_attributes_raw(name, (&raw mut attributes).cast::<c_void>()) }
}

/// 进程令牌句柄，析构时关闭。
struct Token(HANDLE);

impl Drop for Token {
    fn drop(&mut self) {
        // SAFETY: 句柄由 OpenProcessToken 返回，只在这里关闭一次。
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// 由系统用 LocalAlloc 分配的字符串，析构时释放。
struct LocalString(PWSTR);

impl Drop for LocalString {
    fn drop(&mut self) {
        // SAFETY: 指针由 ConvertSidToStringSidW 分配，只在这里释放一次。
        let _ = unsafe { LocalFree(Some(HLOCAL(self.0.0.cast()))) };
    }
}

/// 由系统用 LocalAlloc 分配的安全描述符，析构时释放。
struct LocalDescriptor(PSECURITY_DESCRIPTOR);

impl Drop for LocalDescriptor {
    fn drop(&mut self) {
        // SAFETY: 指针由 ConvertStringSecurityDescriptorToSecurityDescriptorW 分配，只在这里释放一次。
        let _ = unsafe { LocalFree(Some(HLOCAL(self.0.0))) };
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
    async fn first_instance_prevents_a_second_server_with_the_same_name() {
        let name = format!(r"\\.\pipe\aem-sys-test-{}", std::process::id());
        let _first = create_pipe_server(&name, true).unwrap();
        assert!(create_pipe_server(&name, true).is_err());
    }
}
