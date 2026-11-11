//! 全局热键与低级输入钩子。
//!
//! 线程模型：`RegisterHotKey` 和 `SetWindowsHookExW` 都绑定调用线程，消息要在这个线程上取，所以由一个专用线程持有。
//! 钩子回调里只把事件投进通道然后立即返回：回调超时的钩子会被 Windows 静默移除。
//! 钩子不吞按键，只在有绑定需要时才安装。

use std::sync::OnceLock;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP, MOD_ALT, MOD_CONTROL,
    MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, RegisterHotKey, SendInput,
    UnregisterHotKey, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, HHOOK, KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT, PM_NOREMOVE, PeekMessageW,
    PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_APP, WM_HOTKEY,
    WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
};

/// `MSLLHOOKSTRUCT::mouseData` 高位字里的侧键编号
const XBUTTON1: u32 = 1;
const XBUTTON2: u32 = 2;
const HIGH_WORD_SHIFT: u32 = 16;
/// 让消息循环去处理命令队列
const WM_WAKE: u32 = WM_APP + 1;

/// 鼠标侧键：X1 通常是"后退"，X2 通常是"前进"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideButton {
    X1,
    X2,
}

/// 从钩子或 `RegisterHotKey` 收到的原始输入。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawInput {
    Key { vk: u32, down: bool },
    Mouse { button: SideButton, down: bool },
    Hotkey(i32),
}

/// `RegisterHotKey` 的修饰键。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HotkeyModifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
}

enum Command {
    Register {
        id: i32,
        modifiers: HotkeyModifiers,
        vk: u32,
        reply: Sender<bool>,
    },
    Unregister(i32),
    SetHooks {
        keyboard: bool,
        mouse: bool,
        reply: Sender<windows::core::Result<()>>,
    },
    Stop,
}

/// 钩子回调用的全局通道。钩子回调没有用户数据参数，只能走全局；整个进程只有一个输入线程。
static RAW_SINK: OnceLock<Sender<RawInput>> = OnceLock::new();

/// 把原始输入交给转发方。钩子回调必须尽快返回，所以这里只发送，不做任何处理。
fn emit(input: RawInput) {
    if let Some(sink) = RAW_SINK.get() {
        let _ = sink.send(input);
    }
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        // SAFETY: WH_KEYBOARD_LL 的 lparam 总是指向有效的 KBDLLHOOKSTRUCT
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        match wparam.0 as u32 {
            WM_KEYDOWN | WM_SYSKEYDOWN => emit(RawInput::Key {
                vk: info.vkCode,
                down: true,
            }),
            WM_KEYUP | WM_SYSKEYUP => emit(RawInput::Key {
                vk: info.vkCode,
                down: false,
            }),
            _ => {}
        }
    }
    // SAFETY: 原样传递给下一个钩子
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let message = wparam.0 as u32;
        if message == WM_XBUTTONDOWN || message == WM_XBUTTONUP {
            // SAFETY: WH_MOUSE_LL 的 lparam 总是指向有效的 MSLLHOOKSTRUCT
            let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
            let button = match info.mouseData >> HIGH_WORD_SHIFT {
                XBUTTON1 => Some(SideButton::X1),
                XBUTTON2 => Some(SideButton::X2),
                _ => None,
            };
            if let Some(button) = button {
                emit(RawInput::Mouse {
                    button,
                    down: message == WM_XBUTTONDOWN,
                });
            }
        }
    }
    // SAFETY: 原样传递给下一个钩子
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// 输入线程的句柄。丢弃时线程注销全部热键和钩子后退出。
#[derive(Debug)]
pub struct InputThread {
    commands: Sender<Command>,
    thread_id: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum InputThreadError {
    #[error("the input thread is already running")]
    AlreadyRunning,
    #[error("cannot start the input thread: {0}")]
    Spawn(#[from] std::io::Error),
}

impl InputThread {
    /// 启动输入线程，返回句柄和原始输入的接收端。整个进程只能启动一次。
    pub fn start() -> Result<(Self, Receiver<RawInput>), InputThreadError> {
        let (sink, inputs) = mpsc::channel();
        RAW_SINK.set(sink).map_err(|_| InputThreadError::AlreadyRunning)?;
        let (commands, receiver) = mpsc::channel();
        let (ready, started) = mpsc::channel();
        thread::Builder::new()
            .name("aemeath-input-hooks".to_owned())
            .spawn(move || run(&receiver, &ready))?;
        let thread_id = started.recv().map_err(std::io::Error::other)?;
        Ok((Self { commands, thread_id }, inputs))
    }

    fn send(&self, command: Command) {
        if self.commands.send(command).is_ok() {
            // SAFETY: thread_id 是输入线程的 ID；线程已退出时调用失败，忽略
            let _ = unsafe { PostThreadMessageW(self.thread_id, WM_WAKE, WPARAM(0), LPARAM(0)) };
        }
    }

    /// 注册全局热键；被其他程序占用时返回 false。
    pub fn register(&self, id: i32, modifiers: HotkeyModifiers, vk: u32) -> bool {
        let (reply, result) = mpsc::channel();
        self.send(Command::Register {
            id,
            modifiers,
            vk,
            reply,
        });
        result.recv().unwrap_or(false)
    }

    /// 注销一个热键；`id` 是注册时用的编号。
    pub fn unregister(&self, id: i32) {
        self.send(Command::Unregister(id));
    }

    /// 按需安装或卸载两个低级钩子。
    pub fn set_hooks(&self, keyboard: bool, mouse: bool) -> windows::core::Result<()> {
        let (reply, result) = mpsc::channel();
        self.send(Command::SetHooks { keyboard, mouse, reply });
        result
            .recv()
            .unwrap_or_else(|_| Err(windows::core::Error::from_thread()))
    }
}

impl Drop for InputThread {
    fn drop(&mut self) {
        self.send(Command::Stop);
    }
}

/// 返回系统实际注入的事件数；被拦截时是 0。
fn send_input(input: INPUT) -> u32 {
    // SAFETY: input 是有效的 INPUT，长度和结构大小都正确
    unsafe { SendInput(&[input], size_of::<INPUT>() as i32) }
}

/// 注入一次键盘按键，只供热键的验证示例使用。返回实际注入的事件数。
pub fn inject_key(vk: u32, down: bool) -> u32 {
    send_input(INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk as u16),
                wScan: 0,
                dwFlags: if down { Default::default() } else { KEYEVENTF_KEYUP },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    })
}

/// 注入一次鼠标侧键，只供热键的验证示例使用。返回实际注入的事件数。
pub fn inject_side_button(button: SideButton, down: bool) -> u32 {
    send_input(INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: match button {
                    SideButton::X1 => XBUTTON1,
                    SideButton::X2 => XBUTTON2,
                },
                dwFlags: if down { MOUSEEVENTF_XDOWN } else { MOUSEEVENTF_XUP },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    })
}

fn native_modifiers(modifiers: HotkeyModifiers) -> HOT_KEY_MODIFIERS {
    let mut native = MOD_NOREPEAT;
    if modifiers.ctrl {
        native |= MOD_CONTROL;
    }
    if modifiers.alt {
        native |= MOD_ALT;
    }
    if modifiers.shift {
        native |= MOD_SHIFT;
    }
    if modifiers.win {
        native |= MOD_WIN;
    }
    native
}

#[derive(Default)]
struct Hooks {
    keyboard: Option<HHOOK>,
    mouse: Option<HHOOK>,
}

impl Hooks {
    /// 按需安装或卸载键盘、鼠标的低级钩子；已是目标状态的不动。
    fn set(&mut self, keyboard: bool, mouse: bool) -> windows::core::Result<()> {
        // SAFETY: 取本进程的模块句柄，不增加引用计数
        let module: HINSTANCE = unsafe { GetModuleHandleW(None)? }.into();
        // SAFETY: 回调是 'static 的 extern 函数；句柄由 Hooks 持有，卸载一次
        unsafe {
            match (keyboard, self.keyboard) {
                (true, None) => {
                    self.keyboard = Some(SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), Some(module), 0)?);
                }
                (false, Some(hook)) => {
                    let _ = UnhookWindowsHookEx(hook);
                    self.keyboard = None;
                }
                _ => {}
            }
            match (mouse, self.mouse) {
                (true, None) => {
                    self.mouse = Some(SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(module), 0)?);
                }
                (false, Some(hook)) => {
                    let _ = UnhookWindowsHookEx(hook);
                    self.mouse = None;
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// 输入线程的消息循环。热键注册和钩子都绑定在线程上，所以全部在这个线程里做；其他线程的命令经消息唤醒后处理。
fn run(commands: &Receiver<Command>, ready: &Sender<u32>) {
    let mut message = MSG::default();
    // SAFETY: 让系统为本线程建立消息队列，之后 PostThreadMessageW 才不会失败
    let _ = unsafe { PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE) };
    // SAFETY: 没有前置条件
    let _ = ready.send(unsafe { GetCurrentThreadId() });
    let mut hooks = Hooks::default();
    let mut registered: Vec<i32> = Vec::new();
    'pump: loop {
        // SAFETY: message 是有效的可写结构；返回 0 表示 WM_QUIT，-1 表示出错
        let result = unsafe { GetMessageW(&mut message, None, 0, 0) };
        if result.0 <= 0 {
            break;
        }
        if message.message == WM_HOTKEY {
            emit(RawInput::Hotkey(message.wParam.0 as i32));
        }
        while let Ok(command) = commands.try_recv() {
            match command {
                Command::Register {
                    id,
                    modifiers,
                    vk,
                    reply,
                } => {
                    // SAFETY: 窗口句柄为空表示 WM_HOTKEY 投递到本线程的队列
                    let done = unsafe { RegisterHotKey(None, id, native_modifiers(modifiers), vk) }.is_ok();
                    if done {
                        registered.push(id);
                    }
                    let _ = reply.send(done);
                }
                Command::Unregister(id) => {
                    // SAFETY: 只注销本线程注册的 ID
                    let _ = unsafe { UnregisterHotKey(None, id) };
                    registered.retain(|own| *own != id);
                }
                Command::SetHooks { keyboard, mouse, reply } => {
                    let _ = reply.send(hooks.set(keyboard, mouse));
                }
                Command::Stop => break 'pump,
            }
        }
    }
    let _ = hooks.set(false, false);
    for id in registered {
        // SAFETY: 同上
        let _ = unsafe { UnregisterHotKey(None, id) };
    }
}
