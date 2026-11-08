//! 设备实例（Configuration Manager）：列出实例 ID、查询父实例。包括当前不在线的实例（卸载残留设备时要用，ADR-0009）。

use std::io;

use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_GETIDLIST_FILTER_ENUMERATOR, CM_Get_Device_ID_List_SizeW, CM_Get_Device_ID_ListW, CM_Get_Device_ID_Size,
    CM_Get_Device_IDW, CM_Get_Parent, CM_LOCATE_DEVNODE_PHANTOM, CM_Locate_DevNodeW, CONFIGRET, CR_NO_SUCH_DEVNODE,
    CR_SUCCESS,
};
use windows::core::HSTRING;

fn check(result: CONFIGRET) -> io::Result<()> {
    if result == CR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::other(format!("Configuration Manager error {}", result.0)))
    }
}

/// 某个枚举器（如 `USB`）下的全部设备实例 ID，包括不在线的。
pub fn instance_ids(enumerator: &str) -> io::Result<Vec<String>> {
    let filter = HSTRING::from(enumerator);
    let mut length = 0u32;
    // SAFETY: 只查询所需长度；filter 在调用期间有效
    check(unsafe { CM_Get_Device_ID_List_SizeW(&mut length, &filter, CM_GETIDLIST_FILTER_ENUMERATOR) })?;
    let mut buffer = vec![0u16; length as usize];
    // SAFETY: 缓冲区正好是上面查询到的长度；系统写入以双 NUL 结尾的多字符串
    check(unsafe { CM_Get_Device_ID_ListW(&filter, &mut buffer, CM_GETIDLIST_FILTER_ENUMERATOR) })?;
    Ok(buffer
        .split(|unit| *unit == 0)
        .filter(|part| !part.is_empty())
        .map(String::from_utf16_lossy)
        .collect())
}

/// 实例的父实例 ID；实例不存在时返回 None。
pub fn parent_instance_id(instance: &str) -> io::Result<Option<String>> {
    let id = HSTRING::from(instance);
    let mut node = 0u32;
    // SAFETY: id 在调用期间有效；PHANTOM 标志让不在线的实例也能找到
    let located = unsafe { CM_Locate_DevNodeW(&mut node, &id, CM_LOCATE_DEVNODE_PHANTOM) };
    if located == CR_NO_SUCH_DEVNODE {
        return Ok(None);
    }
    check(located)?;
    let mut parent = 0u32;
    // SAFETY: node 是上面找到的有效节点
    check(unsafe { CM_Get_Parent(&mut parent, node, 0) })?;
    let mut length = 0u32;
    // SAFETY: parent 有效，只查询长度（不含结尾的 NUL）
    check(unsafe { CM_Get_Device_ID_Size(&mut length, parent, 0) })?;
    let mut buffer = vec![0u16; length as usize + 1];
    // SAFETY: 缓冲区比 ID 长度多一个字符，容纳结尾的 NUL
    check(unsafe { CM_Get_Device_IDW(parent, &mut buffer, 0) })?;
    let end = buffer.iter().position(|unit| *unit == 0).unwrap_or(buffer.len());
    Ok(Some(String::from_utf16_lossy(&buffer[..end])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_software_devices_and_finds_their_parents() {
        // 每台 Windows 都有 ROOT 枚举器下的设备；它们的父节点是设备树的根
        let ids = instance_ids("ROOT").unwrap();
        assert!(!ids.is_empty());
        assert!(parent_instance_id(&ids[0]).unwrap().is_some());
        assert_eq!(parent_instance_id(r"ROOT\AEMEATH_NO_SUCH_DEVICE\0000").unwrap(), None);
    }
}
