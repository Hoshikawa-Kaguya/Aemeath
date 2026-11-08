//! 音频端点的枚举、状态查询与变化通知（MMDevice API）。
//!
//! 线程模型：[`AudioEndpoints`] 和 [`EndpointWatcher`] 持有 COM 接口指针，只能在创建它们的线程上使用，
//! 该线程须持有 [`crate::com::ComApartment`]。通知回调在系统的工作线程上调用，只把事件交给调用方的回调函数。

use windows::Win32::Devices::FunctionDiscovery::{PKEY_Device_FriendlyName, PKEY_DeviceInterface_FriendlyName};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    DEVICE_STATE, DEVICE_STATE_ACTIVE, DEVICE_STATE_DISABLED, DEVICE_STATE_NOTPRESENT, DEVICE_STATE_UNPLUGGED,
    DEVICE_STATEMASK_ALL, EDataFlow, ERole, IMMDevice, IMMDeviceEnumerator, IMMEndpoint, IMMNotificationClient,
    IMMNotificationClient_Impl, MMDeviceEnumerator, eCapture, eRender,
};
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, STGM_READ};
use windows::core::{HSTRING, Interface, PCWSTR, PWSTR, implement};

/// 端点的数据方向。注意槽位方向与之相反：虚拟输入（In）是播放端点，虚拟输出（Out）是录音端点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioFlow {
    /// 播放端点（扬声器）
    Render,
    /// 录音端点（麦克风、线路）
    Capture,
}

/// 端点状态（`DEVICE_STATE_*`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EndpointState {
    Active,
    Disabled,
    /// 设备不在线（例如 USB/IP 设备已卸下）
    NotPresent,
    /// 插孔类设备未插入
    Unplugged,
}

impl EndpointState {
    fn from_raw(state: DEVICE_STATE) -> Option<Self> {
        match state {
            DEVICE_STATE_ACTIVE => Some(Self::Active),
            DEVICE_STATE_DISABLED => Some(Self::Disabled),
            DEVICE_STATE_NOTPRESENT => Some(Self::NotPresent),
            DEVICE_STATE_UNPLUGGED => Some(Self::Unplugged),
            _ => None,
        }
    }
}

/// 一个端点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointInfo {
    /// 端点 ID（`IMMDevice::GetId`），设备实例不变时永久不变
    pub id: String,
    pub flow: AudioFlow,
    pub state: EndpointState,
    /// 端点名，例如"扬声器 (Aemeath Dev In 1)"（`PKEY_Device_FriendlyName`）
    pub name: Option<String>,
    /// 设备接口名，USB 设备是产品名，例如"Aemeath Dev In 1"（`PKEY_DeviceInterface_FriendlyName`）
    pub interface_name: Option<String>,
}

/// 端点枚举器。
pub struct AudioEndpoints {
    enumerator: IMMDeviceEnumerator,
}

impl std::fmt::Debug for AudioEndpoints {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioEndpoints").finish_non_exhaustive()
    }
}

impl AudioEndpoints {
    pub fn new() -> windows::core::Result<Self> {
        // SAFETY: 标准的 COM 创建调用；调用线程须已初始化 COM（见模块说明）
        let enumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
        Ok(Self { enumerator })
    }

    /// 某个方向的全部端点，包括不在线、已禁用的（不在线的槽位也要能找到）。
    pub fn list(&self, flow: AudioFlow) -> windows::core::Result<Vec<EndpointInfo>> {
        // SAFETY: 枚举器有效；状态掩码是文档规定的取值
        let collection = unsafe {
            self.enumerator
                .EnumAudioEndpoints(raw_flow(flow), DEVICE_STATE(DEVICE_STATEMASK_ALL))?
        };
        // SAFETY: collection 有效
        let count = unsafe { collection.GetCount()? };
        let mut endpoints = Vec::new();
        for index in 0..count {
            // SAFETY: index 小于 GetCount 的结果
            let device = unsafe { collection.Item(index)? };
            if let Some(info) = describe(&device, Some(flow))? {
                endpoints.push(info);
            }
        }
        Ok(endpoints)
    }

    /// 按端点 ID 查询；端点不存在时返回 None。
    pub fn get(&self, id: &str) -> windows::core::Result<Option<EndpointInfo>> {
        let id = HSTRING::from(id);
        // SAFETY: id 在调用期间有效
        match unsafe { self.enumerator.GetDevice(&id) } {
            Ok(device) => describe(&device, None),
            Err(error) if is_not_found(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// 注册变化通知。`on_change` 在系统的工作线程上调用，不能阻塞（只应把事件转发到通道）。
    pub fn watch(
        &self,
        on_change: impl Fn(EndpointChange) + Send + Sync + 'static,
    ) -> windows::core::Result<EndpointWatcher> {
        let client: IMMNotificationClient = NotificationSink {
            on_change: Box::new(on_change),
        }
        .into();
        // SAFETY: client 由 EndpointWatcher 持有，并在其 Drop 中先注销再释放
        unsafe { self.enumerator.RegisterEndpointNotificationCallback(&client)? };
        Ok(EndpointWatcher {
            enumerator: self.enumerator.clone(),
            client,
        })
    }
}

/// 端点的变化。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointChange {
    Added { id: String },
    Removed { id: String },
    StateChanged { id: String, state: Option<EndpointState> },
}

/// 变化通知的注册；Drop 时注销。
pub struct EndpointWatcher {
    enumerator: IMMDeviceEnumerator,
    client: IMMNotificationClient,
}

impl std::fmt::Debug for EndpointWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EndpointWatcher").finish_non_exhaustive()
    }
}

impl Drop for EndpointWatcher {
    fn drop(&mut self) {
        // SAFETY: client 是 watch 中注册的同一个对象；注销后系统不再调用它
        let _ = unsafe { self.enumerator.UnregisterEndpointNotificationCallback(&self.client) };
    }
}

#[implement(IMMNotificationClient)]
struct NotificationSink {
    on_change: Box<dyn Fn(EndpointChange) + Send + Sync>,
}

impl IMMNotificationClient_Impl for NotificationSink_Impl {
    #[allow(non_snake_case)]
    fn OnDeviceStateChanged(&self, id: &PCWSTR, state: DEVICE_STATE) -> windows::core::Result<()> {
        if let Some(id) = read_id(id) {
            (self.on_change)(EndpointChange::StateChanged {
                id,
                state: EndpointState::from_raw(state),
            });
        }
        Ok(())
    }

    #[allow(non_snake_case)]
    fn OnDeviceAdded(&self, id: &PCWSTR) -> windows::core::Result<()> {
        if let Some(id) = read_id(id) {
            (self.on_change)(EndpointChange::Added { id });
        }
        Ok(())
    }

    #[allow(non_snake_case)]
    fn OnDeviceRemoved(&self, id: &PCWSTR) -> windows::core::Result<()> {
        if let Some(id) = read_id(id) {
            (self.on_change)(EndpointChange::Removed { id });
        }
        Ok(())
    }

    #[allow(non_snake_case)]
    fn OnDefaultDeviceChanged(&self, _flow: EDataFlow, _role: ERole, _id: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }

    #[allow(non_snake_case)]
    /// 属性变化非常频繁（音量、格式等），这里不关心
    fn OnPropertyValueChanged(&self, _id: &PCWSTR, _key: &PROPERTYKEY) -> windows::core::Result<()> {
        Ok(())
    }
}

fn read_id(id: &PCWSTR) -> Option<String> {
    if id.is_null() {
        return None;
    }
    // SAFETY: 系统传入的是以 NUL 结尾的有效字符串，只在回调期间读取
    unsafe { id.to_string() }.ok()
}

fn raw_flow(flow: AudioFlow) -> EDataFlow {
    match flow {
        AudioFlow::Render => eRender,
        AudioFlow::Capture => eCapture,
    }
}

/// E_NOTFOUND：HRESULT_FROM_WIN32(ERROR_NOT_FOUND)
const E_NOTFOUND: windows::core::HRESULT = windows::core::HRESULT(0x8007_0490_u32 as i32);

fn is_not_found(error: &windows::core::Error) -> bool {
    error.code() == E_NOTFOUND
}

/// 读取端点的 ID、方向、状态和名称。状态无法识别时返回 None（跳过）。
fn describe(device: &IMMDevice, known_flow: Option<AudioFlow>) -> windows::core::Result<Option<EndpointInfo>> {
    // SAFETY: device 有效；GetId 返回的字符串由 take_co_string 释放
    let id = take_co_string(unsafe { device.GetId()? })?;
    // SAFETY: device 有效
    let Some(state) = EndpointState::from_raw(unsafe { device.GetState()? }) else {
        return Ok(None);
    };
    let flow = match known_flow {
        Some(flow) => flow,
        None => {
            let endpoint: IMMEndpoint = device.cast()?;
            // SAFETY: endpoint 有效
            match unsafe { endpoint.GetDataFlow()? } {
                flow if flow == eRender => AudioFlow::Render,
                flow if flow == eCapture => AudioFlow::Capture,
                _ => return Ok(None),
            }
        }
    };
    // 不在线的端点也能读到名称；个别属性缺失时只是没有名称
    // SAFETY: device 有效，只读打开
    let (name, interface_name) = match unsafe { device.OpenPropertyStore(STGM_READ) } {
        Ok(store) => (
            read_string(&store, &PKEY_Device_FriendlyName),
            read_string(&store, &PKEY_DeviceInterface_FriendlyName),
        ),
        Err(_) => (None, None),
    };
    Ok(Some(EndpointInfo {
        id,
        flow,
        state,
        name,
        interface_name,
    }))
}

fn read_string(
    store: &windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore,
    key: &PROPERTYKEY,
) -> Option<String> {
    // SAFETY: store 有效，key 指向静态常量
    let mut value = unsafe { store.GetValue(key) }.ok()?;
    // SAFETY: value 是 GetValue 返回的有效 PROPVARIANT；返回的字符串由 take_co_string 释放
    let text = unsafe { PropVariantToStringAlloc(&value) }
        .ok()
        .and_then(|text| take_co_string(text).ok());
    // SAFETY: value 由 GetValue 初始化，清理后不再使用
    let _ = unsafe { PropVariantClear(&mut value) };
    text.filter(|text| !text.is_empty())
}

/// 把 CoTaskMemAlloc 分配的字符串复制出来并释放。
fn take_co_string(text: PWSTR) -> windows::core::Result<String> {
    if text.is_null() {
        return Ok(String::new());
    }
    // SAFETY: text 是以 NUL 结尾的有效字符串
    let copied = unsafe { text.to_string() };
    // SAFETY: text 由 COM 用 CoTaskMemAlloc 分配，所有权已转给调用方，此后不再使用
    unsafe { CoTaskMemFree(Some(text.0 as *const _)) };
    copied.map_err(|_| windows::core::Error::from(windows::Win32::Foundation::E_UNEXPECTED))
}
