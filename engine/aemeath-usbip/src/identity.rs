//! 槽位的固定 USB 身份（ADR-0009）。
//!
//! Windows 按"VID/PID + 序列号"识别设备实例，端点 ID、设备名、共享格式都绑定在实例上，
//! 所以这里的每一项都必须永久不变：改动任何一项都会产生新的设备实例，OBS 等应用里选好的设备随之失效。
//!
//! 身份还决定了引擎与主程序共用的几个默认名称（数据目录、命名管道），也放在这里，两个进程各自计算、结果一致。

use std::io;
use std::path::PathBuf;

use aemeath_protocol::ids::{SlotDirection, SlotId, SlotNumber};

use crate::wire::{BusId, WireError};

/// 使用哪一套身份。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityProfile {
    /// 开发与测试用：pid.codes 的测试 PID，设备名 `Aemeath Dev In/Out N`，不与正式安装的设备重名。
    /// 与 S3 注册的开发槽位完全一致，可以复用已有的设备实例。
    ///
    /// 正式身份要等向 pid.codes 申请到正式 PID 后再加入（M4），在那之前不能发布。
    Development,
}

/// usbip-win2 默认连接的端口，也是 Aemeath USB/IP 服务端的默认端口
pub const DEFAULT_USBIP_PORT: u16 = 3241;
const APP_DATA_VARIABLE: &str = "APPDATA";

impl IdentityProfile {
    /// 默认数据目录：`%APPDATA%\Aemeath`；开发身份用 `%APPDATA%\Aemeath Dev`，与正式安装互不干扰
    /// （ADR-0012；便携版的 `data\` 由守护进程传入）。
    pub fn default_data_dir(self) -> Option<PathBuf> {
        let app_data = std::env::var_os(APP_DATA_VARIABLE)?;
        let folder = match self {
            Self::Development => "Aemeath Dev",
        };
        Some(PathBuf::from(app_data).join(folder))
    }

    /// 引擎的命名管道名，带上当前用户的 SID：同一台电脑的不同用户各自运行一个引擎，互不冲突。
    pub fn engine_pipe_name(self) -> io::Result<String> {
        let sid = aemeath_sys::pipe::current_user_sid()?;
        let product = match self {
            Self::Development => "Aemeath.Dev",
        };
        Ok(format!(r"\\.\pipe\{product}.Engine.{sid}"))
    }

    /// 电平共享内存的名称（`Local\` 命名空间：引擎和主程序在同一个登录会话里），同样带上用户 SID。
    pub fn meter_memory_name(self) -> io::Result<String> {
        let sid = aemeath_sys::pipe::current_user_sid()?;
        let product = match self {
            Self::Development => "Aemeath.Dev",
        };
        Ok(format!(r"Local\{product}.Meters.{sid}"))
    }
}

/// pid.codes 的公共 VID
const VENDOR_ID: u16 = 0x1209;
/// pid.codes 的测试 PID：0x0001 用于输入槽位（Windows 里的播放设备），0x0002 用于输出槽位（录音设备）
const DEV_PRODUCT_ID_IN: u16 = 0x0001;
const DEV_PRODUCT_ID_OUT: u16 = 0x0002;
/// 设备版本号。Windows 会按"VID/PID + 版本号"缓存部分描述符，所以描述符有不兼容的改动时要改这个值。
/// 0x0203 与 S3 的开发槽位一致。
const DEVICE_RELEASE: u16 = 0x0203;
/// 输出槽位的 busid 编号接在 8 个输入槽位之后：In N → `1-N`，Out N → `1-(8+N)`
const OUT_BUS_OFFSET: u8 = SlotNumber::LAST;

/// 一个槽位的完整身份。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotIdentity {
    pub slot: SlotId,
    pub busid: BusId,
    /// USB/IP 的设备号，与 busid 的编号相同
    pub device_number: u32,
    pub vendor_id: u16,
    pub product_id: u16,
    pub device_release: u16,
    pub manufacturer: String,
    /// 设备名：Windows 显示为"扬声器 (Aemeath Dev In 1)"等
    pub product_name: String,
    pub serial_number: String,
}

impl IdentityProfile {
    pub fn identity(self, slot: SlotId) -> Result<SlotIdentity, WireError> {
        let number = slot.number.get();
        let (bus_number, product_id, name_direction, serial_direction) = match slot.direction {
            SlotDirection::In => (number, DEV_PRODUCT_ID_IN, "In", "IN"),
            SlotDirection::Out => (OUT_BUS_OFFSET + number, DEV_PRODUCT_ID_OUT, "Out", "OUT"),
        };
        Ok(SlotIdentity {
            slot,
            busid: BusId::new(&format!("1-{bus_number}"))?,
            device_number: u32::from(bus_number),
            vendor_id: VENDOR_ID,
            product_id,
            device_release: DEVICE_RELEASE,
            manufacturer: "Aemeath".to_owned(),
            product_name: format!("Aemeath Dev {name_direction} {number}"),
            serial_number: format!("AEMDEV{serial_direction}{number:02}"),
        })
    }

    /// 全部 16 个槽位的身份，先输入后输出。
    pub fn all(self) -> Result<Vec<SlotIdentity>, WireError> {
        all_slots().map(|slot| self.identity(slot)).collect()
    }
}

/// 全部 16 个槽位，先输入后输出。
pub fn all_slots() -> impl Iterator<Item = SlotId> {
    [SlotDirection::In, SlotDirection::Out]
        .into_iter()
        .flat_map(SlotId::all)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(direction: SlotDirection, number: u8) -> SlotId {
        SlotId {
            direction,
            number: SlotNumber::new(number).unwrap(),
        }
    }

    /// 与 S3 注册的开发槽位逐项一致（改动会产生新的设备实例）
    #[test]
    fn development_identities_match_the_registered_s3_slots() {
        let in1 = IdentityProfile::Development
            .identity(slot(SlotDirection::In, 1))
            .unwrap();
        assert_eq!(in1.busid.as_str(), "1-1");
        assert_eq!(
            (in1.vendor_id, in1.product_id, in1.device_release),
            (0x1209, 0x0001, 0x0203)
        );
        assert_eq!(in1.product_name, "Aemeath Dev In 1");
        assert_eq!(in1.serial_number, "AEMDEVIN01");

        let out8 = IdentityProfile::Development
            .identity(slot(SlotDirection::Out, 8))
            .unwrap();
        assert_eq!(out8.busid.as_str(), "1-16");
        assert_eq!(out8.device_number, 16);
        assert_eq!(out8.product_id, 0x0002);
        assert_eq!(out8.product_name, "Aemeath Dev Out 8");
        assert_eq!(out8.serial_number, "AEMDEVOUT08");
    }

    #[test]
    fn sixteen_slots_with_unique_bus_ids_and_serials() {
        let all = IdentityProfile::Development.all().unwrap();
        assert_eq!(all.len(), 16);
        let busids: std::collections::HashSet<_> = all.iter().map(|identity| identity.busid.clone()).collect();
        let serials: std::collections::HashSet<_> = all.iter().map(|identity| identity.serial_number.clone()).collect();
        assert_eq!((busids.len(), serials.len()), (16, 16));
    }
}
