//! 只允许当前用户访问的安全属性（ADR-0008：IPC 只允许当前用户），命名管道与共享内存共用。

use std::io;

use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::core::HSTRING;

use crate::pipe::current_user_sid;

/// 安全属性及其描述符。描述符由系统分配，析构时释放；[`Self::attributes`] 返回的指针在本对象存活期间有效。
pub(crate) struct CurrentUserOnly {
    descriptor: PSECURITY_DESCRIPTOR,
    attributes: SECURITY_ATTRIBUTES,
}

impl CurrentUserOnly {
    pub(crate) fn new() -> io::Result<Self> {
        let sid = current_user_sid()?;
        // 受保护的 DACL（P），只有一条：允许当前用户完全访问（GA）。其他用户（包括管理员）都无权打开。
        let sddl = HSTRING::from(format!("D:P(A;;GA;;;{sid})"));
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: sddl 是有效的宽字符串；成功后 descriptor 指向系统分配的描述符，由 Drop 释放。
        unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(&sddl, SDDL_REVISION_1, &mut descriptor, None) }?;
        Ok(Self {
            descriptor,
            attributes: SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor.0,
                bInheritHandle: false.into(),
            },
        })
    }

    /// 传给 Create* 函数的 SECURITY_ATTRIBUTES。系统只在调用时读取，不保留指针。
    pub(crate) fn attributes(&mut self) -> *mut SECURITY_ATTRIBUTES {
        &raw mut self.attributes
    }
}

impl Drop for CurrentUserOnly {
    fn drop(&mut self) {
        // SAFETY: 指针由 ConvertStringSecurityDescriptorToSecurityDescriptorW 分配，只在这里释放一次。
        let _ = unsafe { LocalFree(Some(HLOCAL(self.descriptor.0))) };
    }
}
