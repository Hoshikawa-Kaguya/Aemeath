//! 播放端点上的音频会话（`IAudioSessionManager2`）：应用页列出正在出声的程序，以及它们的声音实际落在哪个端点
//! （GLOSSARY：落点）。
//!
//! 线程模型：每次调用都新建 COM 对象，调用线程须持有 [`crate::com::ComApartment`]。

use std::path::PathBuf;

use windows::Win32::Foundation::{CloseHandle, MAX_PATH, S_OK};
use windows::Win32::Media::Audio::{
    AudioSessionStateExpired, DEVICE_STATE_ACTIVE, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator,
    MMDeviceEnumerator, eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::core::{Interface, PWSTR};

use crate::audio_endpoints::take_co_string;

/// 一个音频会话。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioSession {
    pub process_id: u32,
    /// 程序的完整路径；进程已退出或无权查询时为 None
    pub executable: Option<PathBuf>,
    /// 程序给会话设的显示名（多数程序不设，为空）
    pub display_name: String,
    /// 会话所在的播放端点（也就是这个程序当前的落点）
    pub endpoint_id: String,
    /// Windows 的系统声音会话
    pub system_sounds: bool,
}

/// 列出所有活动播放端点上的会话（不含已过期的）。
pub fn render_sessions() -> windows::core::Result<Vec<AudioSession>> {
    // SAFETY: 标准的 COM 创建调用；调用线程须已初始化 COM（见模块说明）
    let enumerator: IMMDeviceEnumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
    // SAFETY: 枚举器有效；只要活动的播放端点
    let devices = unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)? };
    // SAFETY: devices 有效
    let count = unsafe { devices.GetCount()? };
    let mut sessions = Vec::new();
    for index in 0..count {
        // SAFETY: index 小于 GetCount 的结果
        let device = unsafe { devices.Item(index)? };
        // SAFETY: device 有效；GetId 返回 CoTaskMemAlloc 分配的字符串，交给 take_co_string 释放
        let endpoint_id = take_co_string(unsafe { device.GetId()? })?;
        // SAFETY: device 有效；IAudioSessionManager2 是播放端点支持的激活接口
        let Ok(manager) = (unsafe { device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) }) else {
            continue;
        };
        // SAFETY: manager 有效
        let list = unsafe { manager.GetSessionEnumerator()? };
        // SAFETY: list 有效
        let session_count = unsafe { list.GetCount()? };
        for session_index in 0..session_count {
            // SAFETY: session_index 小于 GetCount 的结果
            let Ok(control) = (unsafe { list.GetSession(session_index) }) else {
                continue;
            };
            let Ok(control) = control.cast::<IAudioSessionControl2>() else {
                continue;
            };
            // SAFETY: control 有效
            if unsafe { control.GetState() }.is_ok_and(|state| state == AudioSessionStateExpired) {
                continue;
            }
            // SAFETY: control 有效
            let process_id = unsafe { control.GetProcessId() }.unwrap_or_default();
            // SAFETY: control 有效；返回 S_OK 表示系统声音会话，S_FALSE 表示不是
            let system_sounds = unsafe { control.IsSystemSoundsSession() } == S_OK;
            // SAFETY: control 有效；返回的字符串交给 take_co_string 释放
            let display_name = unsafe { control.GetDisplayName() }
                .and_then(take_co_string)
                .unwrap_or_default();
            sessions.push(AudioSession {
                process_id,
                executable: executable_of(process_id),
                display_name,
                endpoint_id: endpoint_id.clone(),
                system_sounds,
            });
        }
    }
    Ok(sessions)
}

/// 进程的完整路径。
fn executable_of(process_id: u32) -> Option<PathBuf> {
    if process_id == 0 {
        return None;
    }
    // SAFETY: 只请求查询信息的最小权限；句柄在下面关闭
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id) }.ok()?;
    let mut buffer = vec![0u16; MAX_PATH as usize * 4];
    let mut length = buffer.len() as u32;
    // SAFETY: buffer 与 length 描述同一块可写缓冲；成功时 length 是写入的字符数
    let result =
        unsafe { QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buffer.as_mut_ptr()), &mut length) };
    // SAFETY: process 是上面打开的有效句柄，此后不再使用
    let _ = unsafe { CloseHandle(process) };
    result.ok()?;
    let path = String::from_utf16(&buffer[..length as usize]).ok()?;
    Some(PathBuf::from(path))
}
