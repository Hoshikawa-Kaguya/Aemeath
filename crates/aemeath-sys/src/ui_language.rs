//! 用户的界面语言列表（守护进程的对话框按它选择中文或英文）。

use windows::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};
use windows::core::PWSTR;

/// 用户偏好的界面语言，按优先级排列，例如 `["zh-CN", "en-US"]`。读取失败时返回空列表。
pub fn preferred_ui_languages() -> Vec<String> {
    let mut count = 0u32;
    let mut length = 0u32;
    // SAFETY: 第一次调用只查询所需长度（不传缓冲区）
    if unsafe { GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, None, &mut length) }.is_err() {
        return Vec::new();
    }
    let mut buffer = vec![0u16; length as usize];
    // SAFETY: 缓冲区正好是上面查询到的长度；系统写入以双 NUL 结尾的多字符串
    let result = unsafe {
        GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            Some(PWSTR(buffer.as_mut_ptr())),
            &mut length,
        )
    };
    if result.is_err() {
        return Vec::new();
    }
    buffer
        .split(|unit| *unit == 0)
        .filter(|part| !part.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_at_least_one_language_tag() {
        let languages = preferred_ui_languages();
        assert!(!languages.is_empty());
        assert!(
            languages.iter().all(|tag| tag.contains('-') || tag.len() == 2),
            "{languages:?}"
        );
    }
}
