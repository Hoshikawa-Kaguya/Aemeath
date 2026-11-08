//! 按应用指派默认播放端点（"应用音量和设备首选项"背后的未公开接口 `IAudioPolicyConfigFactory`，S7）。
//!
//! S7 的实测约束：
//! - 指派按可执行文件持久保存，对这个程序以后的每次启动都生效；
//! - 读写都要求目标进程正在运行并且已有音频会话，否则返回 `E_INVALIDARG`；
//! - 已在播放的流不一定被转移：启动时就把默认设备解析成具体端点的程序，要重启后才生效；
//! - 清除就是写入空 HSTRING。
//!
//! 只支持 build 21390 起的接口 ID（Aemeath 只支持 Windows 11）。接口布局参考 EarTrumpet（MIT）的公开声明。
//! 线程模型：[`AppRouting`] 持有 COM 接口指针，只能在创建它的线程上使用（须持有 [`crate::com::ComApartment`]）。

use std::ffi::c_void;

use windows::Win32::Media::Audio::{EDataFlow, ERole, eConsole, eMultimedia, eRender};
use windows::Win32::System::WinRT::RoGetActivationFactory;
use windows::core::{HRESULT, HSTRING, IUnknown, IUnknown_Vtbl, interface};

const ACTIVATABLE_CLASS: &str = "Windows.Media.Internal.AudioPolicyConfig";
/// 端点 ID（`{0.0.x.00000000}.{guid}`）要包装成 MMDEVAPI 设备接口路径才能传给该接口
const MMDEVAPI_PREFIX: &str = r"\\?\SWD#MMDEVAPI#";
const RENDER_INTERFACE_SUFFIX: &str = "#{e6327cad-dcec-4949-ae8a-991e976a79d2}";
/// 与 Windows 设置页一致：同时设置 Console 与 Multimedia 两个角色
const ASSIGNED_ROLES: [ERole; 2] = [eConsole, eMultimedia];

/// 方法顺序就是虚表顺序，不能调整；不用的方法只占位置，绝不调用。
/// 它本是 IInspectable 接口；`#[interface]` 不支持以 IInspectable 为基接口，所以按 IUnknown 声明，
/// 并把 IInspectable 的 3 个方法写成占位（与 S7 的 C# 声明相同）。
#[interface("ab3d4648-e242-459f-b02f-541c70306324")]
unsafe trait IAudioPolicyConfigFactory: IUnknown {
    fn get_iids(&self) -> HRESULT;
    fn get_runtime_class_name(&self) -> HRESULT;
    fn get_trust_level(&self) -> HRESULT;
    fn add_ctx_volume_change(&self) -> HRESULT;
    fn remove_ctx_volume_changed(&self) -> HRESULT;
    fn add_ringer_vibrate_state_changed(&self) -> HRESULT;
    fn remove_ringer_vibrate_state_change(&self) -> HRESULT;
    fn set_volume_group_gain_for_id(&self) -> HRESULT;
    fn get_volume_group_gain_for_id(&self) -> HRESULT;
    fn get_active_volume_group_for_endpoint_id(&self) -> HRESULT;
    fn get_volume_groups_for_endpoint(&self) -> HRESULT;
    fn get_current_volume_context(&self) -> HRESULT;
    fn set_volume_group_mute_for_id(&self) -> HRESULT;
    fn get_volume_group_mute_for_id(&self) -> HRESULT;
    fn set_ringer_vibrate_state(&self) -> HRESULT;
    fn get_ringer_vibrate_state(&self) -> HRESULT;
    fn set_preferred_chat_application(&self) -> HRESULT;
    fn reset_preferred_chat_application(&self) -> HRESULT;
    fn get_preferred_chat_application(&self) -> HRESULT;
    fn get_current_chat_applications(&self) -> HRESULT;
    fn add_chat_context_changed(&self) -> HRESULT;
    fn remove_chat_context_changed(&self) -> HRESULT;
    /// `device_id` 是 HSTRING（设备接口路径）；空 HSTRING（null）表示恢复为跟随系统默认
    fn set_persisted_default_audio_endpoint(
        &self,
        process_id: u32,
        flow: EDataFlow,
        role: ERole,
        device_id: *mut c_void,
    ) -> HRESULT;
    /// `device_id` 收到设备接口路径；没有指派时是空串
    fn get_persisted_default_audio_endpoint(
        &self,
        process_id: u32,
        flow: EDataFlow,
        role: ERole,
        device_id: *mut HSTRING,
    ) -> HRESULT;
}

pub struct AppRouting {
    factory: IAudioPolicyConfigFactory,
}

impl std::fmt::Debug for AppRouting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppRouting").finish_non_exhaustive()
    }
}

impl AppRouting {
    pub fn new() -> windows::core::Result<Self> {
        // SAFETY: 标准的 WinRT 激活工厂调用；调用线程须已初始化 COM（见模块说明）
        let factory = unsafe { RoGetActivationFactory(&HSTRING::from(ACTIVATABLE_CLASS))? };
        Ok(Self { factory })
    }

    /// 把进程的默认播放端点指派为 `endpoint_id`；为 None 时恢复为跟随系统默认。
    /// 目标进程必须正在运行并且已有音频会话。
    pub fn set_render_endpoint(&self, process_id: u32, endpoint_id: Option<&str>) -> windows::core::Result<()> {
        let path = match endpoint_id {
            Some(id) => HSTRING::from(format!("{MMDEVAPI_PREFIX}{id}{RENDER_INTERFACE_SUFFIX}")),
            None => HSTRING::new(),
        };
        for role in ASSIGNED_ROLES {
            // SAFETY: HSTRING 是对 HSTRING 句柄的 repr(transparent) 包装，按值复制出句柄（空串是 null）。
            // 被调方不接管所有权，path 在调用期间一直有效，之后由 path 自己释放
            let raw: *mut c_void = unsafe { std::mem::transmute_copy(&path) };
            // SAFETY: factory 有效；process_id、flow、role 都是合法值
            unsafe {
                self.factory
                    .set_persisted_default_audio_endpoint(process_id, eRender, role, raw)
            }
            .ok()?;
        }
        Ok(())
    }

    /// 进程当前的持久指派（端点 ID）；没有指派时为 None。目标进程必须正在运行并且已有音频会话。
    pub fn render_endpoint(&self, process_id: u32) -> windows::core::Result<Option<String>> {
        let mut path = HSTRING::new();
        // SAFETY: factory 有效；path 是可写的 HSTRING 输出参数，所有权转给它，由 path 释放
        unsafe {
            self.factory
                .get_persisted_default_audio_endpoint(process_id, eRender, eConsole, &raw mut path)
        }
        .ok()?;
        Ok(endpoint_id_of(&path.to_string_lossy()))
    }
}

/// 从设备接口路径里取出端点 ID；不是预期的格式时按没有指派处理。
fn endpoint_id_of(path: &str) -> Option<String> {
    path.strip_prefix(MMDEVAPI_PREFIX)?
        .strip_suffix(RENDER_INTERFACE_SUFFIX)
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_the_endpoint_id_from_the_device_path() {
        let path = format!("{MMDEVAPI_PREFIX}{{0.0.0.00000000}}.{{abc}}{RENDER_INTERFACE_SUFFIX}");
        assert_eq!(endpoint_id_of(&path).as_deref(), Some("{0.0.0.00000000}.{abc}"));
        assert_eq!(endpoint_id_of(""), None);
    }
}
