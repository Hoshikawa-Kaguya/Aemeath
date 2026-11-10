//! 读写注册表：守护进程检查 usbip-win2 的安装版本（ADR-0011），写 WER 的崩溃转储设置（需要管理员，M4 计划 W2）。

use std::io;

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_SET_VALUE, REG_DWORD, REG_EXPAND_SZ,
    REG_OPTION_NON_VOLATILE, REG_SZ, REG_VALUE_TYPE, RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW,
    RegDeleteTreeW, RegGetValueW, RegOpenKeyExW, RegSetValueExW,
};
use windows::core::HSTRING;

const WCHAR_BYTES: u32 = 2;

fn check(status: WIN32_ERROR) -> io::Result<()> {
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(status.0 as i32))
    }
}

/// `HKEY_LOCAL_MACHINE\<subkey>` 是否存在。
pub fn local_machine_key_exists(subkey: &str) -> io::Result<bool> {
    let subkey = HSTRING::from(subkey);
    let mut key = HKEY::default();
    // SAFETY: 名称在调用期间有效；输出是一个有效的 HKEY
    let status = unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, &subkey, None, KEY_READ, &mut key) };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(false);
    }
    check(status)?;
    // SAFETY: key 刚打开成功，只在这里关闭一次
    let _ = unsafe { RegCloseKey(key) };
    Ok(true)
}

fn set_value(root: HKEY, subkey: &str, value: &str, kind: REG_VALUE_TYPE, data: &[u8]) -> io::Result<()> {
    let subkey = HSTRING::from(subkey);
    let value = HSTRING::from(value);
    let mut key = HKEY::default();
    // SAFETY: 名称在调用期间有效；输出是一个有效的 HKEY
    check(unsafe {
        RegCreateKeyExW(
            root,
            &subkey,
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
    })?;
    // SAFETY: key 刚打开成功；data 在调用期间有效
    let status = unsafe { RegSetValueExW(key, &value, None, kind, Some(data)) };
    // SAFETY: key 刚打开成功，只在这里关闭一次
    let _ = unsafe { RegCloseKey(key) };
    check(status)
}

fn wide_bytes(data: &str) -> Vec<u8> {
    data.encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// 写 `REG_EXPAND_SZ` 值，键不存在时创建（需要管理员）。
pub fn set_local_machine_expand_string(subkey: &str, value: &str, data: &str) -> io::Result<()> {
    set_value(HKEY_LOCAL_MACHINE, subkey, value, REG_EXPAND_SZ, &wide_bytes(data))
}

/// 写 `REG_DWORD` 值，键不存在时创建（需要管理员）。
pub fn set_local_machine_dword(subkey: &str, value: &str, data: u32) -> io::Result<()> {
    set_value(HKEY_LOCAL_MACHINE, subkey, value, REG_DWORD, &data.to_le_bytes())
}

/// 写当前用户的 `REG_SZ` 值，键不存在时创建。
pub fn set_current_user_string(subkey: &str, value: &str, data: &str) -> io::Result<()> {
    set_value(HKEY_CURRENT_USER, subkey, value, REG_SZ, &wide_bytes(data))
}

/// 删除当前用户的一个值；键或值不存在时视为成功。
pub fn delete_current_user_value(subkey: &str, value: &str) -> io::Result<()> {
    let subkey = HSTRING::from(subkey);
    let value = HSTRING::from(value);
    // SAFETY: 名称在调用期间有效
    let status = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, &subkey, &value) };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    check(status)
}

/// 删除 `HKEY_LOCAL_MACHINE\<subkey>` 及其子项；不存在时视为成功（需要管理员）。
pub fn delete_local_machine_tree(subkey: &str) -> io::Result<()> {
    let subkey = HSTRING::from(subkey);
    // SAFETY: 名称在调用期间有效
    let status = unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, &subkey) };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    check(status)
}

/// 读取 `HKEY_LOCAL_MACHINE\<subkey>` 下的字符串值；键或值不存在时返回 None。
pub fn local_machine_string(subkey: &str, value: &str) -> io::Result<Option<String>> {
    read_string(HKEY_LOCAL_MACHINE, subkey, value)
}

/// 读取当前用户的字符串值；键或值不存在时返回 None。
pub fn current_user_string(subkey: &str, value: &str) -> io::Result<Option<String>> {
    read_string(HKEY_CURRENT_USER, subkey, value)
}

fn read_string(root: HKEY, subkey: &str, value: &str) -> io::Result<Option<String>> {
    let subkey = HSTRING::from(subkey);
    let value = HSTRING::from(value);
    let mut bytes = 0u32;
    // SAFETY: 第一次调用只查询所需字节数（不传缓冲区）；两个名称在调用期间有效
    let status = unsafe { RegGetValueW(root, &subkey, &value, RRF_RT_REG_SZ, None, None, Some(&mut bytes)) };
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
            root,
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

    #[test]
    fn a_current_user_value_round_trips_and_deleting_twice_is_fine() {
        let key = format!(r"Software\AemeathRegistryTest{}", std::process::id());
        set_current_user_string(&key, "Name", r#""C: b\c.exe" --x"#).unwrap();
        let stored = current_user_string(&key, "Name");
        let deleted = delete_current_user_value(&key, "Name");
        let deleted_again = delete_current_user_value(&key, "Name");
        let after = current_user_string(&key, "Name");
        // SAFETY: 名称在调用期间有效
        let _ = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(&key)) };
        assert_eq!(stored.unwrap().as_deref(), Some(r#""C: b\c.exe" --x"#));
        deleted.unwrap();
        deleted_again.unwrap();
        assert_eq!(after.unwrap(), None);
    }

    #[test]
    fn tells_whether_a_key_exists() {
        assert!(local_machine_key_exists(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion").unwrap());
        assert!(!local_machine_key_exists(r"SOFTWARE\Aemeath\Missing").unwrap());
    }
}
