//! 热键在界面上的表示（字段名为 camelCase）。

use aemeath_core::hotkeys::{Hotkey, HotkeyBinding, HotkeyMode, HotkeyTarget, KeyCode, Modifiers};
use aemeath_protocol::ids::NodeId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum KeyCodeView {
    Keyboard { vk: u32 },
    MouseX1,
    MouseX2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyView {
    /// 修饰键和下面的生效方式直接用领域类型，JSON 与界面的类型一致（见 `view.rs` 的测试）
    pub modifiers: Modifiers,
    pub key: KeyCodeView,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyBindingView {
    pub key: HotkeyView,
    pub mode: HotkeyMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum HotkeyTargetView {
    NodeMute { node: u32 },
    RouteToggle { from: u32, to: u32 },
    NodeRecord { node: u32 },
    AllRecording,
    PanicMute,
    ToggleWindow,
}

impl From<Hotkey> for HotkeyView {
    fn from(hotkey: Hotkey) -> Self {
        Self {
            modifiers: hotkey.modifiers,
            key: match hotkey.key {
                KeyCode::Keyboard { vk } => KeyCodeView::Keyboard { vk },
                KeyCode::MouseX1 => KeyCodeView::MouseX1,
                KeyCode::MouseX2 => KeyCodeView::MouseX2,
            },
        }
    }
}

impl From<HotkeyView> for Hotkey {
    fn from(view: HotkeyView) -> Self {
        Self {
            modifiers: view.modifiers,
            key: match view.key {
                KeyCodeView::Keyboard { vk } => KeyCode::Keyboard { vk },
                KeyCodeView::MouseX1 => KeyCode::MouseX1,
                KeyCodeView::MouseX2 => KeyCode::MouseX2,
            },
        }
    }
}

impl From<HotkeyBinding> for HotkeyBindingView {
    fn from(binding: HotkeyBinding) -> Self {
        Self {
            key: binding.key.into(),
            mode: binding.mode,
        }
    }
}

impl From<HotkeyBindingView> for HotkeyBinding {
    fn from(view: HotkeyBindingView) -> Self {
        Self {
            key: view.key.into(),
            mode: view.mode,
        }
    }
}

impl From<HotkeyTarget> for HotkeyTargetView {
    fn from(target: HotkeyTarget) -> Self {
        match target {
            HotkeyTarget::NodeMute(node) => Self::NodeMute { node: node.0 },
            HotkeyTarget::RouteToggle(from, to) => Self::RouteToggle { from: from.0, to: to.0 },
            HotkeyTarget::NodeRecord(node) => Self::NodeRecord { node: node.0 },
            HotkeyTarget::AllRecording => Self::AllRecording,
            HotkeyTarget::PanicMute => Self::PanicMute,
            HotkeyTarget::ToggleWindow => Self::ToggleWindow,
        }
    }
}

impl From<HotkeyTargetView> for HotkeyTarget {
    fn from(view: HotkeyTargetView) -> Self {
        match view {
            HotkeyTargetView::NodeMute { node } => Self::NodeMute(NodeId(node)),
            HotkeyTargetView::RouteToggle { from, to } => Self::RouteToggle(NodeId(from), NodeId(to)),
            HotkeyTargetView::NodeRecord { node } => Self::NodeRecord(NodeId(node)),
            HotkeyTargetView::AllRecording => Self::AllRecording,
            HotkeyTargetView::PanicMute => Self::PanicMute,
            HotkeyTargetView::ToggleWindow => Self::ToggleWindow,
        }
    }
}

/// 热键页的一行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyRowView {
    pub target: HotkeyTargetView,
    pub binding: HotkeyBindingView,
    /// `RegisterHotKey` 失败：被其他程序占用
    pub occupied: bool,
}
