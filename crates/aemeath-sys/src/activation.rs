//! 唤起已在运行的主程序窗口，以及主程序在任务栏上的身份。
//!
//! 主程序持有一个命名事件（自动复位），在后台线程等待它；重复启动的进程触发它，主程序收到后显示主窗口。
//! 这个事件同时标记"主程序已在运行"：创建时发现同名事件已存在，就说明已有实例。

use std::io;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, GetLastError, HANDLE, WAIT_OBJECT_0,
};
use windows::Win32::System::Threading::{
    CreateEventW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent, WaitForSingleObject,
};
use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
use windows::core::HSTRING;

use crate::security::CurrentUserOnly;

/// 主程序持有的唤起事件。持有期间，同名的 [`ActivationEvent::create`] 都返回 None。
#[derive(Debug)]
pub struct ActivationEvent(HANDLE);

// SAFETY: 事件句柄可以在任意线程等待和关闭
unsafe impl Send for ActivationEvent {}

impl ActivationEvent {
    /// 创建唤起事件；同名事件已存在（已有实例在运行）时返回 None。
    /// 事件只允许当前用户访问（名称建议带上用户 SID）。
    pub fn create(name: &str) -> io::Result<Option<Self>> {
        let mut security = CurrentUserOnly::new()?;
        let name = HSTRING::from(name);
        // SAFETY: 安全属性和名称在调用期间有效；自动复位、初始为未触发
        let handle = unsafe { CreateEventW(Some(security.attributes().cast_const()), false, false, &name) }?;
        // SAFETY: 紧接在 CreateEventW 之后读取它设置的错误码
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            // SAFETY: handle 有效，不再使用
            let _ = unsafe { CloseHandle(handle) };
            return Ok(None);
        }
        Ok(Some(Self(handle)))
    }

    /// 阻塞到下一次被触发。自动复位：等待之前的多次触发只算一次。
    pub fn wait(&self) -> io::Result<()> {
        // SAFETY: 句柄在 self 存活期间有效
        let result = unsafe { WaitForSingleObject(self.0, INFINITE) };
        if result == WAIT_OBJECT_0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

impl Drop for ActivationEvent {
    fn drop(&mut self) {
        // SAFETY: 句柄由 CreateEventW 返回，只在这里关闭一次
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// 触发唤起事件，请已在运行的实例显示窗口。返回 false 表示没有实例在运行（事件不存在）。
pub fn request_activation(name: &str) -> io::Result<bool> {
    let name = HSTRING::from(name);
    // SAFETY: 名称在调用期间有效；只申请触发事件所需的权限
    let handle = match unsafe { OpenEventW(EVENT_MODIFY_STATE, false, &name) } {
        Ok(handle) => handle,
        Err(error) if error.code() == ERROR_FILE_NOT_FOUND.to_hresult() => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    // SAFETY: handle 有效
    let signaled = unsafe { SetEvent(handle) };
    // SAFETY: handle 有效，不再使用
    let _ = unsafe { CloseHandle(handle) };
    signaled?;
    Ok(true)
}

/// 设置当前进程的 AppUserModelID。任务栏按它给窗口归组：与开始菜单快捷方式相同时，
/// 窗口和快捷方式归为一组，用户固定窗口时固定的是那个快捷方式。必须在创建任何窗口之前调用。
pub fn set_app_user_model_id(id: &str) -> io::Result<()> {
    let id = HSTRING::from(id);
    // SAFETY: id 在调用期间有效，以 NUL 结尾
    unsafe { SetCurrentProcessExplicitAppUserModelID(&id) }?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 第二个实例创建不到事件，但能触发第一个；第一个退出后不再有人接收
    #[test]
    fn a_second_instance_activates_the_first() {
        let name = format!(r"Local\aemeath-sys-test-activation-{}", std::process::id());
        assert!(!request_activation(&name).unwrap(), "nobody is running yet");
        let first = ActivationEvent::create(&name).unwrap().unwrap();
        assert!(ActivationEvent::create(&name).unwrap().is_none());
        assert!(request_activation(&name).unwrap());
        // 已经触发过：立即返回
        first.wait().unwrap();
        drop(first);
        assert!(!request_activation(&name).unwrap());
    }
}
