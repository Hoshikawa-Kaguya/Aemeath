//! 热键服务：把绑定列表注册到系统，把原始输入翻译成被触发的热键。
//!
//! 键盘按键用 `RegisterHotKey`（会吞掉按键，所以必须带修饰键）；鼠标侧键用低级鼠标钩子（不吞键），
//! 带修饰键的侧键另外要低级键盘钩子跟踪修饰键。钩子只在有侧键绑定时才安装，没有时进程不挂任何全局钩子。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use aemeath_core::hotkeys::{Hotkey, HotkeyBinding, HotkeyTarget, KeyCode, Modifiers};
use aemeath_sys::input_hooks::{HotkeyModifiers, InputThread, InputThreadError, RawInput, SideButton, is_key_down};
use tokio::sync::mpsc;
use tracing::warn;

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
/// 有按着的修饰键时，每隔这么久查一次真实的按键状态
const RECONCILE_INTERVAL: Duration = Duration::from_millis(200);
/// RegisterHotKey 的 ID 从这里开始编号
const FIRST_REGISTRATION_ID: i32 = 1;

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

/// 从原始输入到被触发的热键的纯逻辑，不碰系统。
#[derive(Default)]
struct Matcher {
    /// 按键组合 → 目标（钩子负责的绑定：鼠标侧键）
    hooked: BTreeMap<Hotkey, HotkeyTarget>,
    /// RegisterHotKey 的 ID → 目标
    registered: BTreeMap<i32, HotkeyTarget>,
    /// 当前按下的修饰键（区分左右，松开一边不影响另一边）
    held_modifiers: BTreeSet<u32>,
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

    /// 绑定的修饰键都按着就算匹配，多按的修饰键不影响：游戏里按住 Shift 奔跑时，侧键热键仍然要能用。
    /// 同时匹配几个时取修饰键最多的那个，否则绑了 X1 之后 Ctrl+X1 永远触发不了。
    fn find(&self, key: KeyCode) -> Option<HotkeyTarget> {
        let current = self.modifiers();
        self.hooked
            .iter()
            .filter(|(hotkey, _)| {
                hotkey.key == key
                    && (!hotkey.modifiers.ctrl || current.ctrl)
                    && (!hotkey.modifiers.alt || current.alt)
                    && (!hotkey.modifiers.shift || current.shift)
                    && (!hotkey.modifiers.win || current.win)
            })
            .max_by_key(|(hotkey, _)| {
                let held = hotkey.modifiers;
                [held.ctrl, held.alt, held.shift, held.win]
                    .into_iter()
                    .filter(|&on| on)
                    .count()
            })
            .map(|(_, target)| *target)
    }

    /// 按真实的按键状态去掉漏掉的修饰键松开：弹出 UAC、锁屏时钩子收不到松开，否则修饰键会一直算作按着。
    /// `is_down` 按虚拟键码查询。
    fn reconcile(&mut self, is_down: impl Fn(u32) -> bool) {
        self.held_modifiers.retain(|vk| is_down(*vk));
    }

    /// 把一条原始输入（`RegisterHotKey` 的消息或钩子事件）翻译成被触发的热键。
    fn on_input(&mut self, input: RawInput) -> Option<HotkeyTarget> {
        match input {
            RawInput::Hotkey(id) => self.registered.get(&id).copied(),
            RawInput::Key { vk, down } if is_modifier(vk) => {
                if down {
                    self.held_modifiers.insert(vk);
                } else {
                    self.held_modifiers.remove(&vk);
                }
                None
            }
            RawInput::Key { .. } | RawInput::Mouse { down: false, .. } => None,
            RawInput::Mouse { button, down: true } => self.find(match button {
                SideButton::X1 => KeyCode::MouseX1,
                SideButton::X2 => KeyCode::MouseX2,
            }),
        }
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
    /// 启动输入线程。一开始没有绑定，由 [`Self::set_bindings`] 设定；被触发的热键送往 `events`。
    pub fn start(events: mpsc::UnboundedSender<HotkeyTarget>) -> Result<Self, InputThreadError> {
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
            .spawn(move || forward(&raw, &shared, &events))?;
        Ok(service)
    }

    /// 替换全部绑定并重新注册，返回被其他程序占用的绑定。
    pub fn set_bindings(&self, bindings: Vec<(HotkeyTarget, HotkeyBinding)>) -> Vec<HotkeyTarget> {
        let mut shared = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        shared.bindings = bindings;
        apply(&mut shared, &self.input);
        shared.occupied.clone()
    }

    /// 界面录入新按键期间暂停全部热键，否则按下的组合会触发操作，`RegisterHotKey` 还会把它吞掉，界面收不到。
    pub fn pause(&self) {
        let mut shared = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        release_all(&mut shared, &self.input);
        // 钩子卸载后看不到修饰键松开，留着会变成永远按住
        shared.matcher.held_modifiers.clear();
        if let Err(error) = self.input.set_hooks(false, false) {
            warn!(%error, "cannot remove the input hooks");
        }
    }

    /// 结束 [`Self::pause`]，按当前绑定重新注册。
    pub fn resume(&self) {
        let mut shared = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        apply(&mut shared, &self.input);
    }
}

/// 注销全部热键，清空匹配状态。
fn release_all(shared: &mut Shared, input: &InputThread) {
    for id in std::mem::take(&mut shared.registered_ids) {
        input.unregister(id);
    }
    shared.matcher.registered.clear();
    shared.matcher.hooked.clear();
}

/// 按当前绑定重新注册 RegisterHotKey 和钩子。
fn apply(shared: &mut Shared, input: &InputThread) {
    release_all(shared, input);
    shared.occupied.clear();
    let mut next_id = FIRST_REGISTRATION_ID;
    let (mut keyboard_hook, mut mouse_hook) = (false, false);
    for (target, binding) in shared.bindings.clone() {
        match binding.key.key {
            KeyCode::Keyboard { vk } => {
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
            KeyCode::MouseX1 | KeyCode::MouseX2 => {
                mouse_hook = true;
                // 带修饰键的鼠标绑定要靠键盘钩子知道修饰键的状态
                keyboard_hook |= binding.key.modifiers.any();
                shared.matcher.hooked.insert(binding.key, target);
            }
        }
    }
    if let Err(error) = input.set_hooks(keyboard_hook, mouse_hook) {
        warn!(%error, "cannot install the input hooks, mouse button hotkeys won't work");
    }
}

/// 转发线程：把输入线程的原始输入翻译后送给控制面。输入线程只管收消息，不在钩子回调里做匹配，免得拖慢系统输入。
/// 有按着的修饰键时定期和真实的按键状态对账。
fn forward(raw: &Receiver<RawInput>, shared: &Mutex<Shared>, events: &mpsc::UnboundedSender<HotkeyTarget>) {
    let mut last_reconcile = Instant::now();
    loop {
        let input = match raw.recv_timeout(RECONCILE_INTERVAL) {
            Ok(input) => Some(input),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let fired = {
            let matcher = &mut shared.lock().unwrap_or_else(PoisonError::into_inner).matcher;
            if !matcher.held_modifiers.is_empty() && last_reconcile.elapsed() >= RECONCILE_INTERVAL {
                matcher.reconcile(is_key_down);
                last_reconcile = Instant::now();
            }
            input.and_then(|input| matcher.on_input(input))
        };
        if let Some(target) = fired {
            let _ = events.send(target);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aemeath_protocol::ids::NodeId;

    fn target(id: u32) -> HotkeyTarget {
        HotkeyTarget::NodeMute(NodeId(id))
    }

    fn side(modifiers: Modifiers, key: KeyCode) -> Hotkey {
        Hotkey { modifiers, key }
    }

    fn ctrl() -> Modifiers {
        Modifiers {
            ctrl: true,
            ..Modifiers::default()
        }
    }

    fn feed(matcher: &mut Matcher, inputs: &[RawInput]) -> Vec<HotkeyTarget> {
        inputs.iter().filter_map(|input| matcher.on_input(*input)).collect()
    }

    fn click(button: SideButton, down: bool) -> RawInput {
        RawInput::Mouse { button, down }
    }

    #[test]
    fn side_buttons_fire_on_press_only() {
        let mut matcher = Matcher::default();
        matcher
            .hooked
            .insert(side(Modifiers::default(), KeyCode::MouseX1), target(3));
        assert_eq!(
            feed(
                &mut matcher,
                &[
                    click(SideButton::X1, true),
                    click(SideButton::X1, false),
                    click(SideButton::X2, true),
                ]
            ),
            vec![target(3)]
        );
    }

    #[test]
    fn the_most_specific_binding_wins_and_extra_modifiers_are_fine() {
        let mut matcher = Matcher::default();
        matcher
            .hooked
            .insert(side(Modifiers::default(), KeyCode::MouseX1), target(1));
        matcher.hooked.insert(side(ctrl(), KeyCode::MouseX1), target(2));
        let key = |vk, down| RawInput::Key { vk, down };
        assert_eq!(
            feed(
                &mut matcher,
                &[
                    key(VK_LCONTROL, true),
                    click(SideButton::X1, true),
                    key(VK_LCONTROL, false),
                    key(VK_LSHIFT, true),
                    click(SideButton::X1, true),
                ]
            ),
            vec![target(2), target(1)]
        );
    }

    #[test]
    fn a_missed_modifier_release_is_made_up_from_the_real_key_state() {
        let mut matcher = Matcher::default();
        matcher
            .hooked
            .insert(side(Modifiers::default(), KeyCode::MouseX1), target(1));
        matcher.hooked.insert(side(ctrl(), KeyCode::MouseX1), target(2));
        feed(
            &mut matcher,
            &[RawInput::Key {
                vk: VK_LCONTROL,
                down: true,
            }],
        );
        // 弹出 UAC 时松开了 Ctrl，钩子没收到
        matcher.reconcile(|_| false);
        assert_eq!(feed(&mut matcher, &[click(SideButton::X1, true)]), vec![target(1)]);
    }

    #[test]
    fn registered_hotkeys_map_their_id_to_the_target() {
        let mut matcher = Matcher::default();
        matcher.registered.insert(7, target(5));
        assert_eq!(feed(&mut matcher, &[RawInput::Hotkey(7)]), vec![target(5)]);
        assert!(feed(&mut matcher, &[RawInput::Hotkey(8)]).is_empty());
    }
}
