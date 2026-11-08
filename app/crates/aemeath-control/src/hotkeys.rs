//! 热键服务（M3 计划 2.6）：把绑定列表注册到系统，把原始输入翻译成"触发、按下、松开"。
//!
//! 键盘的切换型用 `RegisterHotKey`（会吞掉按键，所以必须带修饰键）；按住型和鼠标侧键用低级钩子（不吞键）。
//! 钩子只在有绑定需要时才安装，没有这类绑定时进程不挂任何全局钩子。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use aemeath_core::hotkeys::{Hotkey, HotkeyBinding, HotkeyMode, HotkeyTarget, KeyCode, Modifiers};
use aemeath_sys::input_hooks::{HotkeyModifiers, InputThread, InputThreadError, RawInput, SideButton};
use tokio::sync::{mpsc, oneshot};
use tracing::warn;

const VK_ESCAPE: u32 = 0x1B;
const VK_BACK: u32 = 0x08;
const VK_SHIFT: u32 = 0x10;
const VK_CONTROL: u32 = 0x11;
const VK_MENU: u32 = 0x12;
const VK_LWIN: u32 = 0x5B;
const VK_RWIN: u32 = 0x5C;
const VK_LSHIFT: u32 = 0xA0;
const VK_RSHIFT: u32 = 0xA1;
const VK_LCONTROL: u32 = 0xA2;
const VK_RCONTROL: u32 = 0xA3;
const VK_LMENU: u32 = 0xA4;
const VK_RMENU: u32 = 0xA5;
/// RegisterHotKey 的 ID 从这里开始编号
const FIRST_REGISTRATION_ID: i32 = 1;

/// 热键事件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    /// 切换型被按下
    Fired(HotkeyTarget),
    /// 按住型被按下
    Pressed(HotkeyTarget),
    /// 按住型被松开
    Released(HotkeyTarget),
}

/// 录入按键的结果（M3 计划 H12）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureResult {
    Captured(Hotkey),
    /// 用户按了 Esc
    Cancelled,
    /// 用户按了 Backspace
    Cleared,
}

fn is_modifier(vk: u32) -> bool {
    matches!(
        vk,
        VK_SHIFT
            | VK_CONTROL
            | VK_MENU
            | VK_LWIN
            | VK_RWIN
            | VK_LSHIFT
            | VK_RSHIFT
            | VK_LCONTROL
            | VK_RCONTROL
            | VK_LMENU
            | VK_RMENU
    )
}

/// 从原始输入到热键事件的纯逻辑，不碰系统。
#[derive(Default)]
struct Matcher {
    /// 按键组合 → 目标（钩子负责的绑定：按住型和鼠标侧键）
    hooked: BTreeMap<Hotkey, (HotkeyTarget, HotkeyMode)>,
    /// RegisterHotKey 的 ID → 目标
    registered: BTreeMap<i32, HotkeyTarget>,
    /// 当前按下的修饰键（区分左右，松开一边不影响另一边）
    held_modifiers: BTreeSet<u32>,
    /// 正被按住的按键组合
    active: BTreeSet<Hotkey>,
    capture: Option<oneshot::Sender<CaptureResult>>,
}

impl Matcher {
    fn modifiers(&self) -> Modifiers {
        let held = |keys: [u32; 3]| keys.iter().any(|key| self.held_modifiers.contains(key));
        Modifiers {
            ctrl: held([VK_CONTROL, VK_LCONTROL, VK_RCONTROL]),
            alt: held([VK_MENU, VK_LMENU, VK_RMENU]),
            shift: held([VK_SHIFT, VK_LSHIFT, VK_RSHIFT]),
            win: held([VK_LWIN, VK_RWIN, VK_RWIN]),
        }
    }

    /// 绑定的修饰键都按着就算匹配，多按的修饰键不影响：游戏里按住 Shift 奔跑时，按键说话仍然要能用。
    fn find(&self, key: KeyCode) -> Option<Hotkey> {
        let current = self.modifiers();
        self.hooked
            .keys()
            .find(|hotkey| {
                hotkey.key == key
                    && (!hotkey.modifiers.ctrl || current.ctrl)
                    && (!hotkey.modifiers.alt || current.alt)
                    && (!hotkey.modifiers.shift || current.shift)
                    && (!hotkey.modifiers.win || current.win)
            })
            .copied()
    }

    fn on_key(&mut self, key: KeyCode, down: bool, events: &mut Vec<HotkeyEvent>) {
        if down {
            let Some(hotkey) = self.find(key) else {
                return;
            };
            // 自动重复的按下事件不算新的按下
            if !self.active.insert(hotkey) {
                return;
            }
            if let Some((target, mode)) = self.hooked.get(&hotkey) {
                events.push(match mode {
                    HotkeyMode::Toggle => HotkeyEvent::Fired(*target),
                    HotkeyMode::Hold => HotkeyEvent::Pressed(*target),
                });
            }
        } else {
            let released: Vec<Hotkey> = self.active.iter().filter(|hotkey| hotkey.key == key).copied().collect();
            for hotkey in released {
                self.active.remove(&hotkey);
                if let Some((target, HotkeyMode::Hold)) = self.hooked.get(&hotkey) {
                    events.push(HotkeyEvent::Released(*target));
                }
            }
        }
    }

    fn try_capture(&mut self, key: KeyCode) -> bool {
        let Some(reply) = self.capture.take() else {
            return false;
        };
        let result = match key {
            KeyCode::Keyboard { vk: VK_ESCAPE } => CaptureResult::Cancelled,
            KeyCode::Keyboard { vk: VK_BACK } => CaptureResult::Cleared,
            key => CaptureResult::Captured(Hotkey {
                modifiers: self.modifiers(),
                key,
            }),
        };
        let _ = reply.send(result);
        true
    }

    fn on_input(&mut self, input: RawInput) -> (Vec<HotkeyEvent>, bool) {
        let mut events = Vec::new();
        let mut capture_done = false;
        match input {
            RawInput::Hotkey(id) => {
                if let Some(target) = self.registered.get(&id) {
                    events.push(HotkeyEvent::Fired(*target));
                }
            }
            RawInput::Key { vk, down } if is_modifier(vk) => {
                if down {
                    self.held_modifiers.insert(vk);
                } else {
                    self.held_modifiers.remove(&vk);
                }
            }
            RawInput::Key { vk, down } => {
                let key = KeyCode::Keyboard { vk };
                if down && self.try_capture(key) {
                    capture_done = true;
                } else {
                    self.on_key(key, down, &mut events);
                }
            }
            RawInput::Mouse { button, down } => {
                let key = match button {
                    SideButton::X1 => KeyCode::MouseX1,
                    SideButton::X2 => KeyCode::MouseX2,
                };
                if down && self.try_capture(key) {
                    capture_done = true;
                } else {
                    self.on_key(key, down, &mut events);
                }
            }
        }
        (events, capture_done)
    }
}

struct Shared {
    matcher: Matcher,
    /// 当前的绑定，录入结束后用来恢复
    bindings: Vec<(HotkeyTarget, HotkeyBinding)>,
    /// `RegisterHotKey` 失败的绑定（被其他程序占用）
    occupied: Vec<HotkeyTarget>,
    registered_ids: Vec<i32>,
}

/// 热键服务的句柄。
pub struct HotkeyService {
    input: Arc<InputThread>,
    shared: Arc<Mutex<Shared>>,
}

impl HotkeyService {
    pub fn start(events: mpsc::UnboundedSender<HotkeyEvent>) -> Result<Self, InputThreadError> {
        let (input, raw) = InputThread::start()?;
        let shared = Arc::new(Mutex::new(Shared {
            matcher: Matcher::default(),
            bindings: Vec::new(),
            occupied: Vec::new(),
            registered_ids: Vec::new(),
        }));
        let input = Arc::new(input);
        let service = Self {
            input: Arc::clone(&input),
            shared: Arc::clone(&shared),
        };
        thread::Builder::new()
            .name("aemeath-hotkeys".to_owned())
            .spawn(move || forward(&raw, &shared, &input, &events))?;
        Ok(service)
    }

    /// 替换全部绑定并重新注册，返回被其他程序占用的绑定。
    pub fn set_bindings(&self, bindings: Vec<(HotkeyTarget, HotkeyBinding)>) -> Vec<HotkeyTarget> {
        let mut shared = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        shared.bindings = bindings;
        apply(&mut shared, &self.input);
        shared.occupied.clone()
    }

    /// 录入下一个按键组合（M3 计划 H12）。录入期间暂停全部热键，否则按下的组合同时会触发操作。
    pub fn capture(&self) -> oneshot::Receiver<CaptureResult> {
        let (reply, result) = oneshot::channel();
        let mut shared = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        release_all(&mut shared, &self.input);
        shared.matcher.capture = Some(reply);
        if let Err(error) = self.input.set_hooks(true, true) {
            warn!(%error, "cannot install the input hooks for capturing a hotkey");
        }
        result
    }

    /// 由界面结束录入（窗口有焦点时界面自己收到了按键，或者用户放弃了录入）。录入已经结束时什么也不做。
    pub fn finish_capture(&self, result: CaptureResult) {
        let mut shared = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(reply) = shared.matcher.capture.take() {
            let _ = reply.send(result);
            apply(&mut shared, &self.input);
        }
    }
}

fn release_all(shared: &mut Shared, input: &InputThread) {
    for id in std::mem::take(&mut shared.registered_ids) {
        input.unregister(id);
    }
    shared.matcher.registered.clear();
    shared.matcher.hooked.clear();
    shared.matcher.active.clear();
}

/// 按当前绑定重新注册 RegisterHotKey 和钩子。
fn apply(shared: &mut Shared, input: &InputThread) {
    release_all(shared, input);
    shared.occupied.clear();
    let mut next_id = FIRST_REGISTRATION_ID;
    let (mut keyboard_hook, mut mouse_hook) = (false, false);
    for (target, binding) in shared.bindings.clone() {
        match (binding.key.key, binding.mode) {
            (KeyCode::Keyboard { vk }, HotkeyMode::Toggle) => {
                let modifiers = binding.key.modifiers;
                let native = HotkeyModifiers {
                    ctrl: modifiers.ctrl,
                    alt: modifiers.alt,
                    shift: modifiers.shift,
                    win: modifiers.win,
                };
                if input.register(next_id, native, vk) {
                    shared.registered_ids.push(next_id);
                    shared.matcher.registered.insert(next_id, target);
                    next_id += 1;
                } else {
                    warn!(?target, vk, "the hotkey is taken by another program");
                    shared.occupied.push(target);
                }
            }
            (KeyCode::Keyboard { .. }, HotkeyMode::Hold) => {
                keyboard_hook = true;
                shared.matcher.hooked.insert(binding.key, (target, binding.mode));
            }
            (KeyCode::MouseX1 | KeyCode::MouseX2, _) => {
                mouse_hook = true;
                // 带修饰键的鼠标绑定要靠键盘钩子知道修饰键的状态
                keyboard_hook |= binding.key.modifiers.any();
                shared.matcher.hooked.insert(binding.key, (target, binding.mode));
            }
        }
    }
    if let Err(error) = input.set_hooks(keyboard_hook, mouse_hook) {
        warn!(%error, "cannot install the input hooks, hold hotkeys and mouse buttons won't work");
    }
}

fn forward(
    raw: &Receiver<RawInput>,
    shared: &Mutex<Shared>,
    input: &InputThread,
    events: &mpsc::UnboundedSender<HotkeyEvent>,
) {
    while let Ok(event) = raw.recv() {
        let mut guard = shared.lock().unwrap_or_else(PoisonError::into_inner);
        let (out, capture_done) = guard.matcher.on_input(event);
        if capture_done {
            apply(&mut guard, input);
        }
        drop(guard);
        for event in out {
            let _ = events.send(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aemeath_protocol::ids::NodeId;

    const F13: u32 = 0x7C;
    const KEY_M: u32 = 0x4D;

    fn key(vk: u32, down: bool) -> RawInput {
        RawInput::Key { vk, down }
    }

    fn bare(vk: u32) -> Hotkey {
        Hotkey {
            modifiers: Modifiers::default(),
            key: KeyCode::Keyboard { vk },
        }
    }

    fn target(id: u32) -> HotkeyTarget {
        HotkeyTarget::NodeMute(NodeId(id))
    }

    fn feed(matcher: &mut Matcher, inputs: &[RawInput]) -> Vec<HotkeyEvent> {
        inputs.iter().flat_map(|input| matcher.on_input(*input).0).collect()
    }

    #[test]
    fn hold_fires_once_despite_key_repeat_and_releases() {
        let mut matcher = Matcher::default();
        matcher.hooked.insert(bare(F13), (target(1), HotkeyMode::Hold));
        assert_eq!(
            feed(&mut matcher, &[key(F13, true), key(F13, true), key(F13, false)]),
            vec![HotkeyEvent::Pressed(target(1)), HotkeyEvent::Released(target(1))]
        );
    }

    #[test]
    fn a_bare_hold_still_works_while_a_modifier_is_held() {
        let mut matcher = Matcher::default();
        matcher.hooked.insert(bare(F13), (target(1), HotkeyMode::Hold));
        assert_eq!(
            feed(&mut matcher, &[key(VK_LSHIFT, true), key(F13, true)]),
            vec![HotkeyEvent::Pressed(target(1))]
        );
    }

    #[test]
    fn a_modified_hold_needs_its_modifier_and_survives_releasing_it_first() {
        let chord = Hotkey {
            modifiers: Modifiers {
                ctrl: true,
                ..Modifiers::default()
            },
            key: KeyCode::Keyboard { vk: KEY_M },
        };
        let mut matcher = Matcher::default();
        matcher.hooked.insert(chord, (target(2), HotkeyMode::Hold));
        assert!(feed(&mut matcher, &[key(KEY_M, true), key(KEY_M, false)]).is_empty());
        assert_eq!(
            feed(
                &mut matcher,
                &[
                    key(VK_LCONTROL, true),
                    key(KEY_M, true),
                    key(VK_LCONTROL, false),
                    key(KEY_M, false)
                ]
            ),
            vec![HotkeyEvent::Pressed(target(2)), HotkeyEvent::Released(target(2))]
        );
    }

    #[test]
    fn mouse_side_buttons_work_in_both_modes() {
        let mut matcher = Matcher::default();
        let side = |key| Hotkey {
            modifiers: Modifiers::default(),
            key,
        };
        matcher
            .hooked
            .insert(side(KeyCode::MouseX1), (target(3), HotkeyMode::Toggle));
        matcher
            .hooked
            .insert(side(KeyCode::MouseX2), (target(4), HotkeyMode::Hold));
        let click = |button, down| RawInput::Mouse { button, down };
        assert_eq!(
            feed(
                &mut matcher,
                &[
                    click(SideButton::X1, true),
                    click(SideButton::X1, false),
                    click(SideButton::X2, true),
                    click(SideButton::X2, false),
                ]
            ),
            vec![
                HotkeyEvent::Fired(target(3)),
                HotkeyEvent::Pressed(target(4)),
                HotkeyEvent::Released(target(4)),
            ]
        );
    }

    #[test]
    fn registered_hotkeys_map_their_id_to_the_target() {
        let mut matcher = Matcher::default();
        matcher.registered.insert(7, target(5));
        assert_eq!(
            feed(&mut matcher, &[RawInput::Hotkey(7)]),
            vec![HotkeyEvent::Fired(target(5))]
        );
        assert!(feed(&mut matcher, &[RawInput::Hotkey(8)]).is_empty());
    }

    #[test]
    fn capture_reports_the_chord_and_understands_escape_and_backspace() {
        let mut matcher = Matcher::default();
        let (reply, mut result) = oneshot::channel();
        matcher.capture = Some(reply);
        let (events, done) = matcher.on_input(key(VK_LCONTROL, true));
        assert!(
            events.is_empty() && !done,
            "a modifier alone doesn't finish the capture"
        );
        let (_, done) = matcher.on_input(key(KEY_M, true));
        assert!(done);
        assert_eq!(
            result.try_recv().unwrap(),
            CaptureResult::Captured(Hotkey {
                modifiers: Modifiers {
                    ctrl: true,
                    ..Modifiers::default()
                },
                key: KeyCode::Keyboard { vk: KEY_M },
            })
        );

        for (vk, expected) in [(VK_ESCAPE, CaptureResult::Cancelled), (VK_BACK, CaptureResult::Cleared)] {
            let (reply, mut result) = oneshot::channel();
            let mut matcher = Matcher {
                capture: Some(reply),
                ..Matcher::default()
            };
            matcher.on_input(key(vk, true));
            assert_eq!(result.try_recv().unwrap(), expected);
        }
    }
}
