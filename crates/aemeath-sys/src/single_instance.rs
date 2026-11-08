//! 每个用户只运行一个实例（守护进程）：用一个命名互斥体占位，进程退出时系统自动释放。

use std::io;

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::HSTRING;

use crate::security::CurrentUserOnly;

/// 持有期间，同名的 [`SingleInstance::acquire`] 都会返回 None。
#[derive(Debug)]
pub struct SingleInstance(HANDLE);

// SAFETY: 互斥体句柄可以在任意线程持有和关闭
unsafe impl Send for SingleInstance {}

impl SingleInstance {
    /// 已有实例时返回 None。互斥体只允许当前用户访问（名称建议带上用户 SID）。
    pub fn acquire(name: &str) -> io::Result<Option<Self>> {
        let mut security = CurrentUserOnly::new()?;
        let name = HSTRING::from(name);
        // SAFETY: 安全属性和名称在调用期间有效；不要求初始所有权，只用"是否已存在"判断
        let handle = unsafe { CreateMutexW(Some(security.attributes().cast_const()), false, &name) }?;
        // SAFETY: 紧接在 CreateMutexW 之后读取它设置的错误码
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            // SAFETY: handle 有效，不再使用
            let _ = unsafe { CloseHandle(handle) };
            return Ok(None);
        }
        Ok(Some(Self(handle)))
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        // SAFETY: 句柄由 CreateMutexW 返回，只在这里关闭一次
        let _ = unsafe { CloseHandle(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_instance_is_refused_until_the_first_is_dropped() {
        let name = format!(r"Local\aemeath-sys-test-instance-{}", std::process::id());
        let first = SingleInstance::acquire(&name).unwrap();
        assert!(first.is_some());
        assert!(SingleInstance::acquire(&name).unwrap().is_none());
        drop(first);
        assert!(SingleInstance::acquire(&name).unwrap().is_some());
    }
}
