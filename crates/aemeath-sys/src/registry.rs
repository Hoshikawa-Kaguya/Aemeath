//! 读取注册表（守护进程检查 usbip-win2 的安装版本，ADR-0011）。

use std::io;

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
use windows::core::HSTRING;

const WCHAR_BYTES: u32 = 2;

/// 读取 `HKEY_LOCAL_MACHINE\<subkey>` 下的字符串值；键或值不存在时返回 None。
pub fn local_machine_string(subkey: &str, value: &str) -> io::Result<Option<String>> {
    let subkey = HSTRING::from(subkey);
    let value = HSTRING::from(value);
    let mut bytes = 0u32;
    // SAFETY: 第一次调用只查询所需字节数（不传缓冲区）；两个名称在调用期间有效
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            &subkey,
            &value,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut bytes),
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status.0 as i32));
    }
    let mut buffer = vec![0u16; bytes.div_ceil(WCHAR_BYTES) as usize];
    let mut size = (buffer.len() as u32) * WCHAR_BYTES;
    // SAFETY: 缓冲区至少 size 字节；系统写入以 NUL 结尾的字符串，并把实际字节数写回 size
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            &subkey,
            &value,
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status.0 as i32));
    }
    let length = buffer.iter().position(|unit| *unit == 0).unwrap_or(buffer.len());
    Ok(Some(String::from_utf16_lossy(&buffer[..length])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_value_every_windows_has_and_reports_missing_ones() {
        let product = local_machine_string(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion", "ProductName").unwrap();
        assert!(product.is_some_and(|name| name.starts_with("Windows")));
        assert_eq!(
            local_machine_string(r"SOFTWARE\Aemeath\Missing", "Nothing").unwrap(),
            None
        );
    }
}
