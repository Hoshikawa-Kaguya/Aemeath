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
    Development,
    /// 正式发布用：`1209:AEEE`，设备名 `Aemeath In/Out N`（ADR-0009，M4 计划 I4、I6）。
    Production,
}

/// Aemeath USB/IP 服务端的默认端口。选在 Windows 默认动态范围（49152 起）之外，低端口更容易被 winnat 划进保留段
pub const DEFAULT_USBIP_PORT: u16 = 32410;
const APP_DATA_VARIABLE: &str = "APPDATA";

impl IdentityProfile {
    /// 这次构建使用的身份：只有打开 `production-identity` feature（打包脚本）才是正式身份（M4 计划 I5）。
    pub const fn current() -> Self {
        if cfg!(feature = "production-identity") {
            Self::Production
        } else {
            Self::Development
        }
    }

    /// 命名管道、互斥体、共享内存名称的前缀
    const fn name_prefix(self) -> &'static str {
        match self {
            Self::Development => "Aemeath.Dev",
            Self::Production => "Aemeath",
        }
    }

    /// 数据目录：`%APPDATA%\Aemeath`；开发身份用 `%APPDATA%\Aemeath Dev`，与正式安装互不干扰。
    /// 安装版和便携版相同，不提供修改（ADR-0012，M4 计划 D1）。
    pub fn default_data_dir(self) -> Option<PathBuf> {
        let app_data = std::env::var_os(APP_DATA_VARIABLE)?;
        Some(PathBuf::from(app_data).join(self.data_folder_name()))
    }

    /// `%APPDATA%` 下的数据目录名
    pub const fn data_folder_name(self) -> &'static str {
        match self {
            Self::Development => "Aemeath Dev",
            Self::Production => "Aemeath",
        }
    }

    /// 引擎的命名管道名，带上当前用户的 SID：同一台电脑的不同用户各自运行一个引擎，互不冲突。
    pub fn engine_pipe_name(self) -> io::Result<String> {
        let sid = aemeath_sys::pipe::current_user_sid()?;
        let product = self.name_prefix();
        Ok(format!(r"\\.\pipe\{product}.Engine.{sid}"))
    }

    /// 守护进程的单实例互斥体名称（每个用户、每个登录会话只运行一个守护进程）。
    pub fn guardian_instance_name(self) -> io::Result<String> {
        let sid = aemeath_sys::pipe::current_user_sid()?;
        let product = self.name_prefix();
        Ok(format!(r"Local\{product}.Guardian.{sid}"))
    }

    /// 电平共享内存的名称（`Local\` 命名空间：引擎和主程序在同一个登录会话里），同样带上用户 SID。
    pub fn meter_memory_name(self) -> io::Result<String> {
        let sid = aemeath_sys::pipe::current_user_sid()?;
        let product = self.name_prefix();
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
/// 正式 PID：输入、输出共用（M4 计划 I4）
const PRODUCTION_PRODUCT_ID: u16 = 0xAEEE;
/// 正式身份靠不同的版本号区分方向，避开 Windows 按"VID/PID + 版本号"的描述符缓存
const PRODUCTION_RELEASE_IN: u16 = 0x0100;
const PRODUCTION_RELEASE_OUT: u16 = 0x0101;
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
        let (bus_number, dev_product_id, production_release, name_direction, serial_direction) = match slot.direction {
            SlotDirection::In => (number, DEV_PRODUCT_ID_IN, PRODUCTION_RELEASE_IN, "In", "IN"),
            SlotDirection::Out => (
                OUT_BUS_OFFSET + number,
                DEV_PRODUCT_ID_OUT,
                PRODUCTION_RELEASE_OUT,
                "Out",
                "OUT",
            ),
        };
        let (product_id, device_release, product_name, serial_number) = match self {
            Self::Development => (
                dev_product_id,
                DEVICE_RELEASE,
                format!("Aemeath Dev {name_direction} {number}"),
                format!("AEMDEV{serial_direction}{number:02}"),
            ),
            Self::Production => (
                PRODUCTION_PRODUCT_ID,
                production_release,
                format!("Aemeath {name_direction} {number}"),
                format!("AEM{serial_direction}{number:02}"),
            ),
        };
        Ok(SlotIdentity {
            slot,
            busid: BusId::new(&format!("1-{bus_number}"))?,
            device_number: u32::from(bus_number),
            vendor_id: VENDOR_ID,
            product_id,
            device_release,
            manufacturer: "Aemeath".to_owned(),
            product_name,
            serial_number,
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
    fn production_identities_share_a_pid_and_differ_by_release() {
        let in1 = IdentityProfile::Production
            .identity(slot(SlotDirection::In, 1))
            .unwrap();
        assert_eq!(
            (in1.vendor_id, in1.product_id, in1.device_release),
            (0x1209, 0xAEEE, 0x0100)
        );
        assert_eq!(in1.product_name, "Aemeath In 1");
        assert_eq!(in1.serial_number, "AEMIN01");

        let out8 = IdentityProfile::Production
            .identity(slot(SlotDirection::Out, 8))
            .unwrap();
        assert_eq!(
            (out8.vendor_id, out8.product_id, out8.device_release),
            (0x1209, 0xAEEE, 0x0101)
        );
        assert_eq!(out8.busid.as_str(), "1-16");
        assert_eq!(out8.product_name, "Aemeath Out 8");
        assert_eq!(out8.serial_number, "AEMOUT08");
    }

    #[test]
    fn the_default_build_uses_the_development_identity() {
        assert_eq!(IdentityProfile::current(), IdentityProfile::Development);
    }

    #[test]
    fn sixteen_slots_with_unique_bus_ids_and_serials() {
        for profile in [IdentityProfile::Development, IdentityProfile::Production] {
            let all = profile.all().unwrap();
            assert_eq!(all.len(), 16);
            let busids: std::collections::HashSet<_> = all.iter().map(|identity| identity.busid.clone()).collect();
            let serials: std::collections::HashSet<_> =
                all.iter().map(|identity| identity.serial_number.clone()).collect();
            assert_eq!((busids.len(), serials.len()), (16, 16));
        }
    }
}
