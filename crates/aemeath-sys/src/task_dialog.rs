//! 原生的 TaskDialog（守护进程的驱动说明和引导安装对话框）。
//!
//! 需要 Common Controls 6：调用方的 exe 必须嵌入启用 Common Controls 6 的清单，否则 TaskDialogIndirect 不存在。

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, S_FALSE, S_OK, WPARAM};
use windows::Win32::UI::Controls::{
    TASKDIALOG_BUTTON, TASKDIALOG_FLAGS, TASKDIALOG_NOTIFICATIONS, TASKDIALOGCONFIG, TASKDIALOGCONFIG_0, TD_ERROR_ICON,
    TD_INFORMATION_ICON, TDCBF_CLOSE_BUTTON, TDCBF_OK_BUTTON, TDF_ALLOW_DIALOG_CANCELLATION, TDF_CALLBACK_TIMER,
    TDF_SHOW_MARQUEE_PROGRESS_BAR, TDF_SIZE_TO_CONTENT, TDM_CLICK_BUTTON, TDM_ENABLE_BUTTON,
    TDM_SET_PROGRESS_BAR_MARQUEE, TDN_BUTTON_CLICKED, TDN_CREATED, TDN_TIMER, TaskDialogIndirect,
};
use windows::Win32::UI::WindowsAndMessaging::{IDCANCEL, IDOK, SendMessageW};
use windows::core::{BOOL, HRESULT, HSTRING, PCWSTR};

const PRIMARY_BUTTON_ID: i32 = 100;
const CLOSE_BUTTON_ID: i32 = 101;
/// 进度条动画的刷新间隔（毫秒，0 表示默认值）
const MARQUEE_DEFAULT_INTERVAL: isize = 0;

/// 带两个按钮的提问对话框的文字。
#[derive(Debug, Clone)]
pub struct ChoiceDialog {
    pub title: String,
    pub heading: String,
    pub content: String,
    /// 底部的补充说明；为空时不显示
    pub footer: String,
    pub primary: String,
    pub close: String,
}

/// 显示提问对话框，用户选主要按钮时返回 true。
pub fn ask(dialog: &ChoiceDialog) -> io::Result<bool> {
    let title = HSTRING::from(dialog.title.as_str());
    let heading = HSTRING::from(dialog.heading.as_str());
    let content = HSTRING::from(dialog.content.as_str());
    let footer = HSTRING::from(dialog.footer.as_str());
    let primary = HSTRING::from(dialog.primary.as_str());
    let close = HSTRING::from(dialog.close.as_str());
    let buttons = [
        TASKDIALOG_BUTTON {
            nButtonID: PRIMARY_BUTTON_ID,
            pszButtonText: PCWSTR(primary.as_ptr()),
        },
        TASKDIALOG_BUTTON {
            nButtonID: CLOSE_BUTTON_ID,
            pszButtonText: PCWSTR(close.as_ptr()),
        },
    ];
    let config = TASKDIALOGCONFIG {
        cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
        dwFlags: TASKDIALOG_FLAGS(TDF_ALLOW_DIALOG_CANCELLATION.0 | TDF_SIZE_TO_CONTENT.0),
        pszWindowTitle: PCWSTR(title.as_ptr()),
        Anonymous1: TASKDIALOGCONFIG_0 {
            pszMainIcon: TD_INFORMATION_ICON,
        },
        pszMainInstruction: PCWSTR(heading.as_ptr()),
        pszContent: PCWSTR(content.as_ptr()),
        pszFooter: if dialog.footer.is_empty() {
            PCWSTR::null()
        } else {
            PCWSTR(footer.as_ptr())
        },
        cButtons: buttons.len() as u32,
        pButtons: buttons.as_ptr(),
        nDefaultButton: PRIMARY_BUTTON_ID,
        ..Default::default()
    };
    let mut pressed = 0i32;
    // SAFETY: config 完整初始化；字符串和按钮数组在调用返回之前一直有效；输出是一个有效的 i32
    unsafe { TaskDialogIndirect(&config, Some(&mut pressed), None, None) }?;
    Ok(pressed == PRIMARY_BUTTON_ID)
}

/// 进度对话框的文字。
#[derive(Debug, Clone)]
pub struct ProgressDialog {
    pub title: String,
    pub heading: String,
    pub content: String,
}

fn send(window: HWND, message: i32, wparam: usize, lparam: isize) -> LRESULT {
    // SAFETY: window 是 TaskDialog 回调传入的窗口，回调期间有效；这些消息的参数都是按值传的整数
    unsafe { SendMessageW(window, message as u32, Some(WPARAM(wparam)), Some(LPARAM(lparam))) }
}

/// `reference` 是 [`run_with_progress`] 传入的完成标志。
unsafe extern "system" fn progress_callback(
    window: HWND,
    notification: TASKDIALOG_NOTIFICATIONS,
    wparam: WPARAM,
    _lparam: LPARAM,
    reference: isize,
) -> HRESULT {
    // SAFETY: reference 是 run_with_progress 里 AtomicBool 的地址，对话框关闭之前一直有效
    let finished = unsafe { &*(reference as *const AtomicBool) }.load(Ordering::Acquire);
    match notification {
        TDN_CREATED => {
            send(window, TDM_SET_PROGRESS_BAR_MARQUEE.0, 1, MARQUEE_DEFAULT_INTERVAL);
            send(window, TDM_ENABLE_BUTTON.0, IDOK.0 as usize, BOOL(0).0 as isize);
            S_OK
        }
        TDN_TIMER if finished => {
            send(window, TDM_CLICK_BUTTON.0, IDOK.0 as usize, 0);
            S_OK
        }
        // 不能取消：标题栏的关闭按钮也走这里
        TDN_BUTTON_CLICKED if !finished && (wparam.0 == IDCANCEL.0 as usize || wparam.0 == IDOK.0 as usize) => S_FALSE,
        _ => S_OK,
    }
}

/// 显示带滚动进度条、不能取消的对话框，同时在另一个线程运行 `work`，完成后关闭对话框并返回结果。
pub fn run_with_progress<T: Send>(dialog: &ProgressDialog, work: impl FnOnce() -> T + Send) -> io::Result<T> {
    let title = HSTRING::from(dialog.title.as_str());
    let heading = HSTRING::from(dialog.heading.as_str());
    let content = HSTRING::from(dialog.content.as_str());
    let finished = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let result = work();
            finished.store(true, Ordering::Release);
            result
        });
        let config = TASKDIALOGCONFIG {
            cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
            dwFlags: TASKDIALOG_FLAGS(TDF_SHOW_MARQUEE_PROGRESS_BAR.0 | TDF_CALLBACK_TIMER.0 | TDF_SIZE_TO_CONTENT.0),
            dwCommonButtons: TDCBF_OK_BUTTON,
            pszWindowTitle: PCWSTR(title.as_ptr()),
            pszMainInstruction: PCWSTR(heading.as_ptr()),
            pszContent: PCWSTR(content.as_ptr()),
            pfCallback: Some(progress_callback),
            lpCallbackData: (&raw const finished) as isize,
            ..Default::default()
        };
        // SAFETY: config 完整初始化；字符串和完成标志在调用返回之前一直有效
        let shown = unsafe { TaskDialogIndirect(&config, None, None, None) };
        let result = worker
            .join()
            .map_err(|_| io::Error::other("the background work panicked"))?;
        shown?;
        Ok(result)
    })
}

/// 对话框的文字。
#[derive(Debug, Clone)]
pub struct ErrorDialog {
    pub title: String,
    /// 主要说明（粗体的一句话）
    pub heading: String,
    pub content: String,
    /// 底部的补充说明（例如诊断信息）；为空时不显示
    pub footer: String,
}

/// 显示一个带错误图标和"关闭"按钮的对话框，直到用户关闭。
pub fn show_error(dialog: &ErrorDialog) -> io::Result<()> {
    let title = HSTRING::from(dialog.title.as_str());
    let heading = HSTRING::from(dialog.heading.as_str());
    let content = HSTRING::from(dialog.content.as_str());
    let footer = HSTRING::from(dialog.footer.as_str());
    let config = TASKDIALOGCONFIG {
        cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
        dwFlags: TASKDIALOG_FLAGS(TDF_ALLOW_DIALOG_CANCELLATION.0 | TDF_SIZE_TO_CONTENT.0),
        dwCommonButtons: TDCBF_CLOSE_BUTTON,
        pszWindowTitle: PCWSTR(title.as_ptr()),
        Anonymous1: TASKDIALOGCONFIG_0 {
            pszMainIcon: TD_ERROR_ICON,
        },
        pszMainInstruction: PCWSTR(heading.as_ptr()),
        pszContent: PCWSTR(content.as_ptr()),
        pszFooter: if dialog.footer.is_empty() {
            PCWSTR::null()
        } else {
            PCWSTR(footer.as_ptr())
        },
        ..Default::default()
    };
    // SAFETY: config 完整初始化；其中的字符串指针由上面的 HSTRING 持有，在对话框关闭（调用返回）之前一直有效
    unsafe { TaskDialogIndirect(&config, None, None, None) }?;
    Ok(())
}
