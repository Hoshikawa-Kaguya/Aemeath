//! 原生的 TaskDialog（守护进程在驱动不可用时的说明对话框，ADR-0011）。
//!
//! 需要 Common Controls 6：调用方的 exe 必须嵌入启用 Common Controls 6 的清单，否则 TaskDialogIndirect 不存在。

use std::io;

use windows::Win32::UI::Controls::{
    TASKDIALOG_FLAGS, TASKDIALOGCONFIG, TASKDIALOGCONFIG_0, TD_ERROR_ICON, TDCBF_CLOSE_BUTTON,
    TDF_ALLOW_DIALOG_CANCELLATION, TDF_SIZE_TO_CONTENT, TaskDialogIndirect,
};
use windows::core::{HSTRING, PCWSTR};

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
