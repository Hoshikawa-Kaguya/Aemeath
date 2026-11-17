//! 在本机上验证热键：用 `SendInput` 注入按键，检查键盘组合键、鼠标侧键和占用检测。
//!
//! 用法：`cargo run -p aemeath-control --example hotkey-check`。

use std::time::Duration;

use aemeath_control::hotkeys::HotkeyService;
use aemeath_core::hotkeys::{Hotkey, HotkeyBinding, HotkeyTarget, KeyCode, Modifiers};
use aemeath_protocol::ids::NodeId;
use aemeath_sys::input_hooks::{SideButton, inject_key, inject_side_button};
use tokio::sync::mpsc;

const VK_CONTROL: u32 = 0x11;
const VK_MENU: u32 = 0x12;
/// F13–F15：键盘上通常没有，注入它们不影响其他程序
const VK_F13: u32 = 0x7C;
const SETTLE: Duration = Duration::from_millis(150);
const EVENT_TIMEOUT: Duration = Duration::from_millis(800);

fn target(id: u32) -> HotkeyTarget {
    HotkeyTarget::NodeMute(NodeId(id))
}

fn chord(modifiers: Modifiers, key: KeyCode) -> HotkeyBinding {
    HotkeyBinding {
        key: Hotkey { modifiers, key },
    }
}

async fn expect(events: &mut mpsc::UnboundedReceiver<HotkeyTarget>, wanted: Option<HotkeyTarget>, name: &str) -> bool {
    let received = tokio::time::timeout(EVENT_TIMEOUT, events.recv()).await.ok().flatten();
    let passed = received == wanted;
    println!("{} {name}: {received:?}", if passed { "OK  " } else { "FAIL" });
    passed
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (sender, mut events) = mpsc::unbounded_channel();
    let service = HotkeyService::start(sender)?;
    let ctrl_alt = Modifiers {
        ctrl: true,
        alt: true,
        ..Modifiers::default()
    };
    let occupied = service.set_bindings(vec![
        (target(1), chord(ctrl_alt, KeyCode::Keyboard { vk: VK_F13 })),
        (target(2), chord(Modifiers::default(), KeyCode::MouseX2)),
    ]);
    println!("occupied: {occupied:?}");
    tokio::time::sleep(SETTLE).await;

    let mut passed = occupied.is_empty();

    println!("injected events: {}", inject_key(VK_CONTROL, true));
    inject_key(VK_MENU, true);
    inject_key(VK_F13, true);
    inject_key(VK_F13, false);
    inject_key(VK_MENU, false);
    inject_key(VK_CONTROL, false);
    passed &= expect(&mut events, Some(target(1)), "keyboard chord (RegisterHotKey)").await;

    inject_side_button(SideButton::X2, true);
    passed &= expect(&mut events, Some(target(2)), "mouse side button pressed").await;
    inject_side_button(SideButton::X2, false);
    passed &= expect(&mut events, None, "mouse side button released, nothing fired").await;

    // 整个进程只能有一个输入线程
    let second = HotkeyService::start(mpsc::unbounded_channel().0);
    println!("second input thread refused: {}", second.is_err());
    passed &= second.is_err();

    drop(service);
    println!("{}", if passed { "PASS" } else { "FAIL" });
    if !passed {
        std::process::exit(1);
    }
    Ok(())
}
