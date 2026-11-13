//! Windows API 与 COM 接口的安全封装。
//!
//! 这是全工程唯一允许 `unsafe` 的 crate：其他 crate 只调用这里提供的安全接口。
//! 每个 `unsafe` 块都必须写 `// SAFETY:` 注释，说明为什么满足调用约定。

pub mod activation;
pub mod app_routing;
pub mod audio_endpoints;
pub mod audio_sessions;
pub mod clock;
pub mod com;
pub mod devices;
pub mod elevation;
pub mod icon;
pub mod input_hooks;
pub mod pipe;
pub mod policy_config;
pub mod process;
pub mod registry;
mod security;
pub mod service;
pub mod shared_memory;
pub mod single_instance;
pub mod task_dialog;
pub mod thread_priority;
pub mod timer;
mod token;
pub mod ui_language;

/// API 填好的 UTF-16 缓冲转成字符串：到第一个 NUL 为止（没有 NUL 就取全部），无效的编码单元换成替换字符。
fn wide_until_nul(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|unit| *unit == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

#[cfg(test)]
mod tests {
    #[test]
    fn wide_strings_end_at_the_first_nul() {
        let text: Vec<u16> = "ab\0cd".encode_utf16().collect();
        assert_eq!(super::wide_until_nul(&text), "ab");
        let whole: Vec<u16> = "abc".encode_utf16().collect();
        assert_eq!(super::wide_until_nul(&whole), "abc");
    }
}
