//! 工具页的"系统默认设备"（GLOSSARY：工具页）：两个方向的活动端点，以及 Windows 当前的默认设备、默认通信设备。
//! 只改 Windows 的设置，不记录、不还原。被 Aemeath 独占的设备不可选。

use aemeath_control::audio_policy::EndpointEntry;
use aemeath_core::model::NodeKind;
use aemeath_core::workspace::Workspace;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DefaultDevicesView {
    /// 播放设备
    pub render: Vec<DefaultDeviceView>,
    /// 录音设备
    pub capture: Vec<DefaultDeviceView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DefaultDeviceView {
    pub endpoint_id: String,
    pub name: String,
    pub is_default: bool,
    pub is_communications: bool,
    /// Aemeath 正在独占这台设备：其他程序打不开它，不能设为默认设备
    pub exclusive_by_aemeath: bool,
}

impl DefaultDevicesView {
    /// 列出两个方向的端点，并标出被 Aemeath 独占、不能设为默认的设备。
    pub fn new(render: Vec<EndpointEntry>, capture: Vec<EndpointEntry>, workspace: &Workspace) -> Self {
        let convert = |entries: Vec<EndpointEntry>, output: bool| -> Vec<DefaultDeviceView> {
            entries
                .into_iter()
                .map(|entry| DefaultDeviceView {
                    exclusive_by_aemeath: held_exclusively(workspace, &entry.id, output),
                    endpoint_id: entry.id,
                    name: entry.name,
                    is_default: entry.is_default,
                    is_communications: entry.is_communications,
                })
                .collect()
        };
        Self {
            render: convert(render, true),
            capture: convert(capture, false),
        }
    }
}

/// 工作台上有外部设备节点以独占方式使用这个端点。
fn held_exclusively(workspace: &Workspace, endpoint_id: &str, output: bool) -> bool {
    workspace.nodes().iter().any(|node| {
        let device = match (&node.kind, output) {
            (NodeKind::ExternalOutput { device }, true)
            | (NodeKind::ExternalInput { device }, false)
            | (NodeKind::ExternalInsert { ret: device, .. }, false) => device,
            _ => return false,
        };
        device.endpoint.0 == endpoint_id
            && node
                .device_settings()
                .is_some_and(|settings| settings.access.is_exclusive())
    })
}
