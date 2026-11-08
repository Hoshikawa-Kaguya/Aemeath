//! 当前进程是否以管理员身份（已提权）运行。卸载设备实例需要提权（ADR-0009）。

use std::ffi::c_void;
use std::io;

use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TokenElevation};

use crate::token::Token;

pub fn is_elevated() -> io::Result<bool> {
    let token = Token::current_process()?;
    let mut elevation = TOKEN_ELEVATION::default();
    let mut returned = 0u32;
    // SAFETY: 句柄有效；输出缓冲区是一个完整的 TOKEN_ELEVATION，大小如实传入
    unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            Some((&raw mut elevation).cast::<c_void>()),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
    }?;
    Ok(elevation.TokenIsElevated != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn can_tell_whether_the_process_is_elevated() {
        // 结果取决于测试的运行方式，这里只要求能查询
        assert!(is_elevated().is_ok());
    }
}
