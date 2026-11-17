//! 热键（GLOSSARY：热键、全局热键、专属热键）：按键组合、绑定、校验与冲突检查，以及紧急静音的临时叠加。
//!
//! 热键一律是切换：按一下取反一次。
//!
//! 专属热键存放在所属的节点或连线上，随所属对象一起删除和恢复；全局热键存放在偏好设置里。

use aemeath_protocol::ids::NodeId;
use aemeath_protocol::topology::{NodeRole, RouteTopology};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::NodeKind;
use crate::preferences::Preferences;
use crate::workspace::Workspace;

/// 修饰键的组合。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Modifiers {
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub alt: bool,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub win: bool,
}

impl Modifiers {
    /// 是否至少按了一个修饰键。
    pub fn any(self) -> bool {
        self.ctrl || self.alt || self.shift || self.win
    }
}

/// 按键：键盘的虚拟键码，或鼠标侧键。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum KeyCode {
    Keyboard { vk: u32 },
    MouseX1,
    MouseX2,
}

/// 一个按键组合：修饰键加一个主键。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hotkey {
    #[serde(default)]
    pub modifiers: Modifiers,
    pub key: KeyCode,
}

/// 全局热键。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalHotkeys {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub all_recording: Option<Hotkey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panic_mute: Option<Hotkey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toggle_window: Option<Hotkey>,
}

/// 热键作用的对象。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HotkeyTarget {
    NodeMute(NodeId),
    RouteToggle(NodeId, NodeId),
    NodeRecord(NodeId),
    AllRecording,
    PanicMute,
    ToggleWindow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum HotkeyError {
    #[error("this object no longer exists")]
    UnknownTarget,
    #[error("a hotkey on the keyboard needs Ctrl, Alt, Shift or Win")]
    ModifierRequired,
    #[error("this key is already used by {0:?}")]
    Conflict(HotkeyTarget),
}

impl Hotkey {
    /// 键盘按键必须带修饰键：系统热键会吞掉按键，单键会让这个键没法正常打字。鼠标侧键可以单独使用。
    pub fn is_valid(self) -> bool {
        !matches!(self.key, KeyCode::Keyboard { .. }) || self.modifiers.any()
    }
}

/// 校验并写入一个绑定；`binding` 为 None 表示清除。
pub fn set_hotkey(
    workspace: &mut Workspace,
    preferences: &mut Preferences,
    target: HotkeyTarget,
    binding: Option<Hotkey>,
) -> Result<(), HotkeyError> {
    if let Some(binding) = binding {
        if !binding.is_valid() {
            return Err(HotkeyError::ModifierRequired);
        }
        let conflict = all_hotkeys(workspace, preferences)
            .into_iter()
            .find(|(other, existing)| *other != target && *existing == binding);
        if let Some((other, _)) = conflict {
            return Err(HotkeyError::Conflict(other));
        }
    }
    let slot = match target {
        HotkeyTarget::AllRecording => &mut preferences.global_hotkeys.all_recording,
        HotkeyTarget::PanicMute => &mut preferences.global_hotkeys.panic_mute,
        HotkeyTarget::ToggleWindow => &mut preferences.global_hotkeys.toggle_window,
        HotkeyTarget::NodeMute(id) => workspace.mute_hotkey_mut(id).ok_or(HotkeyError::UnknownTarget)?,
        HotkeyTarget::NodeRecord(id) => workspace.record_hotkey_mut(id).ok_or(HotkeyError::UnknownTarget)?,
        HotkeyTarget::RouteToggle(from, to) => {
            workspace.route_hotkey_mut(from, to).ok_or(HotkeyError::UnknownTarget)?
        }
    };
    *slot = binding;
    Ok(())
}

/// 当前全部绑定。
pub fn all_hotkeys(workspace: &Workspace, preferences: &Preferences) -> Vec<(HotkeyTarget, Hotkey)> {
    let global = preferences.global_hotkeys;
    let mut list: Vec<(HotkeyTarget, Hotkey)> = [
        (HotkeyTarget::AllRecording, global.all_recording),
        (HotkeyTarget::PanicMute, global.panic_mute),
        (HotkeyTarget::ToggleWindow, global.toggle_window),
    ]
    .into_iter()
    .filter_map(|(target, binding)| Some((target, binding?)))
    .collect();
    for node in workspace.nodes() {
        if let Some(binding) = node.mute_hotkey {
            list.push((HotkeyTarget::NodeMute(node.id), binding));
        }
        if let Some(binding) = node.record_hotkey
            && matches!(node.kind, NodeKind::Recorder { .. })
        {
            list.push((HotkeyTarget::NodeRecord(node.id), binding));
        }
    }
    for route in workspace.routes() {
        if let Some(binding) = route.toggle_hotkey {
            list.push((HotkeyTarget::RouteToggle(route.from, route.to), binding));
        }
    }
    list
}

/// 临时叠加：编译拓扑时叠加，不写进配置。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HotkeyOverlay {
    /// 紧急静音：全部输出节点静音
    pub panic_mute: bool,
}

impl HotkeyOverlay {
    /// 叠加到刚编译出的拓扑上：紧急静音时所有输出都静音。
    pub fn apply(&self, topology: &mut RouteTopology) {
        if !self.panic_mute {
            return;
        }
        for node in &mut topology.nodes {
            if node.kind.role() == NodeRole::Output {
                node.strip.muted = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EngineSettings, NodeName, Position};
    use crate::workspace::NodeSpec;
    use aemeath_protocol::ids::TopologyRevision;
    use aemeath_protocol::topology::RecorderSettings;

    const F13: u32 = 0x7C;
    const KEY_M: u32 = 0x4D;

    fn binding(modifiers: Modifiers, key: KeyCode) -> Hotkey {
        Hotkey { modifiers, key }
    }

    fn ctrl() -> Modifiers {
        Modifiers {
            ctrl: true,
            ..Modifiers::default()
        }
    }

    struct Fixture {
        workspace: Workspace,
        preferences: Preferences,
        mic: NodeId,
        out: NodeId,
        recorder: NodeId,
    }

    fn fixture() -> Fixture {
        let mut workspace = Workspace::new(EngineSettings::default());
        let name = NodeName::new("n").unwrap();
        let mic = workspace
            .add_node(name.clone(), NodeSpec::VirtualInput(None), Position::default())
            .unwrap();
        let out = workspace
            .add_node(name.clone(), NodeSpec::VirtualOutput(None), Position::default())
            .unwrap();
        let recorder = workspace
            .add_node(
                name,
                NodeSpec::Recorder(RecorderSettings::default()),
                Position::default(),
            )
            .unwrap();
        workspace.connect(mic, out).unwrap();
        Fixture {
            workspace,
            preferences: Preferences::default(),
            mic,
            out,
            recorder,
        }
    }

    #[test]
    fn keyboard_keys_need_a_modifier_but_mouse_buttons_do_not() {
        let Fixture {
            mut workspace,
            mut preferences,
            mic,
            recorder,
            ..
        } = fixture();
        let bare = KeyCode::Keyboard { vk: F13 };
        assert_eq!(
            set_hotkey(
                &mut workspace,
                &mut preferences,
                HotkeyTarget::NodeMute(mic),
                Some(binding(Modifiers::default(), bare))
            ),
            Err(HotkeyError::ModifierRequired)
        );
        set_hotkey(
            &mut workspace,
            &mut preferences,
            HotkeyTarget::NodeRecord(recorder),
            Some(binding(ctrl(), bare)),
        )
        .unwrap();
        set_hotkey(
            &mut workspace,
            &mut preferences,
            HotkeyTarget::PanicMute,
            Some(binding(Modifiers::default(), KeyCode::MouseX1)),
        )
        .unwrap();
    }

    #[test]
    fn a_key_cannot_be_bound_twice_and_rebinding_the_same_target_is_fine() {
        let Fixture {
            mut workspace,
            mut preferences,
            mic,
            out,
            ..
        } = fixture();
        let key = binding(ctrl(), KeyCode::Keyboard { vk: KEY_M });
        let first = HotkeyTarget::NodeMute(mic);
        set_hotkey(&mut workspace, &mut preferences, first, Some(key)).unwrap();
        set_hotkey(&mut workspace, &mut preferences, first, Some(key)).unwrap();
        assert_eq!(
            set_hotkey(&mut workspace, &mut preferences, HotkeyTarget::NodeMute(out), Some(key)),
            Err(HotkeyError::Conflict(first))
        );
        set_hotkey(&mut workspace, &mut preferences, first, None).unwrap();
        assert!(all_hotkeys(&workspace, &preferences).is_empty());
    }

    #[test]
    fn hotkeys_go_away_with_their_owner() {
        let Fixture {
            mut workspace,
            mut preferences,
            mic,
            out,
            ..
        } = fixture();
        let key = binding(ctrl(), KeyCode::Keyboard { vk: KEY_M });
        set_hotkey(
            &mut workspace,
            &mut preferences,
            HotkeyTarget::RouteToggle(mic, out),
            Some(key),
        )
        .unwrap();
        assert_eq!(all_hotkeys(&workspace, &preferences).len(), 1);
        workspace.remove_node(mic).unwrap();
        assert!(all_hotkeys(&workspace, &preferences).is_empty());
    }

    #[test]
    fn panic_mute_mutes_outputs_only() {
        let Fixture { workspace, .. } = fixture();
        let mut topology = workspace.compile(TopologyRevision(1));
        HotkeyOverlay::default().apply(&mut topology);
        assert!(!topology.nodes[1].strip.muted);
        HotkeyOverlay { panic_mute: true }.apply(&mut topology);
        assert!(!topology.nodes[0].strip.muted, "inputs stay as they are");
        assert!(topology.nodes[1].strip.muted);
    }
}
