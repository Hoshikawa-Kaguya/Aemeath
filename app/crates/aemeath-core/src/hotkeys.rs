//! 热键（GLOSSARY：热键、全局热键、专属热键；M3 计划 1.5）：按键组合、绑定、校验与冲突检查，以及按住型的临时叠加。
//!
//! 专属热键存放在所属的节点或连线上，随所属对象一起删除和恢复；全局热键存放在偏好设置里。

use std::collections::BTreeSet;

use aemeath_protocol::ids::NodeId;
use aemeath_protocol::topology::{NodeRole, RouteTopology};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::NodeKind;
use crate::preferences::Preferences;
use crate::workspace::Workspace;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hotkey {
    #[serde(default)]
    pub modifiers: Modifiers,
    pub key: KeyCode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HotkeyMode {
    /// 按一下取反
    Toggle,
    /// 按住期间取反，松开后恢复；不写进配置
    Hold,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HotkeyBinding {
    pub key: Hotkey,
    pub mode: HotkeyMode,
}

/// 全局热键（M3 计划 H3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalHotkeys {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub all_recording: Option<HotkeyBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panic_mute: Option<HotkeyBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toggle_window: Option<HotkeyBinding>,
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

impl HotkeyTarget {
    /// 录音和显示/隐藏主窗口只能用切换（M3 计划 H4）。
    fn toggle_only(self) -> bool {
        matches!(self, Self::NodeRecord(_) | Self::AllRecording | Self::ToggleWindow)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum HotkeyError {
    #[error("this object no longer exists")]
    UnknownTarget,
    #[error("a toggle hotkey on the keyboard needs Ctrl, Alt, Shift or Win")]
    ModifierRequired,
    #[error("this action can only be a toggle")]
    ToggleOnly,
    #[error("this key is already used by {0:?}")]
    Conflict(HotkeyTarget),
}

/// 校验并写入一个绑定；`binding` 为 None 表示清除。
pub fn set_hotkey(
    workspace: &mut Workspace,
    preferences: &mut Preferences,
    target: HotkeyTarget,
    binding: Option<HotkeyBinding>,
) -> Result<(), HotkeyError> {
    if let Some(binding) = binding {
        if binding.mode == HotkeyMode::Toggle
            && matches!(binding.key.key, KeyCode::Keyboard { .. })
            && !binding.key.modifiers.any()
        {
            return Err(HotkeyError::ModifierRequired);
        }
        if binding.mode == HotkeyMode::Hold && target.toggle_only() {
            return Err(HotkeyError::ToggleOnly);
        }
        let conflict = all_hotkeys(workspace, preferences)
            .into_iter()
            .find(|(other, existing)| *other != target && existing.key == binding.key);
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
pub fn all_hotkeys(workspace: &Workspace, preferences: &Preferences) -> Vec<(HotkeyTarget, HotkeyBinding)> {
    let global = preferences.global_hotkeys;
    let mut list: Vec<(HotkeyTarget, HotkeyBinding)> = [
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

/// 临时叠加（M3 计划 H10）：编译拓扑时叠加，不写进配置，也不进撤销记录。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HotkeyOverlay {
    /// 紧急静音：全部输出节点静音
    pub panic_mute: bool,
    /// 正被按住的热键
    pub held: BTreeSet<HotkeyTarget>,
}

impl HotkeyOverlay {
    pub fn apply(&self, topology: &mut RouteTopology) {
        for node in &mut topology.nodes {
            if self.held.contains(&HotkeyTarget::NodeMute(node.id)) {
                node.strip.muted = !node.strip.muted;
            }
            if self.panic_mute && node.kind.role() == NodeRole::Output {
                node.strip.muted = true;
            }
        }
        for route in &mut topology.routes {
            if self.held.contains(&HotkeyTarget::RouteToggle(route.from, route.to)) {
                route.enabled = !route.enabled;
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

    fn binding(modifiers: Modifiers, key: KeyCode, mode: HotkeyMode) -> HotkeyBinding {
        HotkeyBinding {
            key: Hotkey { modifiers, key },
            mode,
        }
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
    fn keyboard_toggles_need_a_modifier_but_holds_do_not() {
        let Fixture {
            mut workspace,
            mut preferences,
            mic,
            ..
        } = fixture();
        let target = HotkeyTarget::NodeMute(mic);
        let bare = KeyCode::Keyboard { vk: F13 };
        assert_eq!(
            set_hotkey(
                &mut workspace,
                &mut preferences,
                target,
                Some(binding(Modifiers::default(), bare, HotkeyMode::Toggle))
            ),
            Err(HotkeyError::ModifierRequired)
        );
        set_hotkey(
            &mut workspace,
            &mut preferences,
            target,
            Some(binding(Modifiers::default(), bare, HotkeyMode::Hold)),
        )
        .unwrap();
        set_hotkey(
            &mut workspace,
            &mut preferences,
            HotkeyTarget::PanicMute,
            Some(binding(Modifiers::default(), KeyCode::MouseX1, HotkeyMode::Toggle)),
        )
        .unwrap();
    }

    #[test]
    fn recording_and_the_window_are_toggle_only() {
        let Fixture {
            mut workspace,
            mut preferences,
            recorder,
            ..
        } = fixture();
        let hold = binding(ctrl(), KeyCode::Keyboard { vk: KEY_M }, HotkeyMode::Hold);
        for target in [
            HotkeyTarget::NodeRecord(recorder),
            HotkeyTarget::AllRecording,
            HotkeyTarget::ToggleWindow,
        ] {
            assert_eq!(
                set_hotkey(&mut workspace, &mut preferences, target, Some(hold)),
                Err(HotkeyError::ToggleOnly)
            );
        }
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
        let key = binding(ctrl(), KeyCode::Keyboard { vk: KEY_M }, HotkeyMode::Toggle);
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
    fn hotkeys_go_away_with_their_owner_and_come_back_on_undo() {
        let Fixture {
            mut workspace,
            mut preferences,
            mic,
            out,
            ..
        } = fixture();
        let key = binding(ctrl(), KeyCode::Keyboard { vk: KEY_M }, HotkeyMode::Toggle);
        set_hotkey(
            &mut workspace,
            &mut preferences,
            HotkeyTarget::RouteToggle(mic, out),
            Some(key),
        )
        .unwrap();
        let before = workspace.clone();
        workspace.remove_node(mic).unwrap();
        assert!(all_hotkeys(&workspace, &preferences).is_empty());
        workspace.restore(before);
        assert_eq!(all_hotkeys(&workspace, &preferences).len(), 1);
    }

    #[test]
    fn the_overlay_inverts_while_held_and_panic_mutes_outputs() {
        let Fixture {
            workspace, mic, out, ..
        } = fixture();
        let mut topology = workspace.compile(TopologyRevision(1));
        let mut overlay = HotkeyOverlay::default();
        overlay.held.insert(HotkeyTarget::NodeMute(mic));
        overlay.held.insert(HotkeyTarget::RouteToggle(mic, out));
        overlay.apply(&mut topology);
        assert!(topology.nodes[0].strip.muted);
        assert!(!topology.routes[0].enabled);

        let mut topology = workspace.compile(TopologyRevision(1));
        HotkeyOverlay {
            panic_mute: true,
            ..HotkeyOverlay::default()
        }
        .apply(&mut topology);
        assert!(!topology.nodes[0].strip.muted, "inputs stay as they are");
        assert!(topology.nodes[1].strip.muted);
    }
}
