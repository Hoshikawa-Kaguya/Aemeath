//! 标识类型。用 newtype 而不是裸数字或字符串，避免把一种 ID 误当成另一种。

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 协议版本号。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProtocolVersion(pub u32);

/// 节点 ID，由主程序分配，在一份路由拓扑内唯一。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub u32);

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "node {}", self.0)
    }
}

/// 请求 ID：主程序发出的命令带上它，引擎的应答原样带回，用来配对。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(pub u64);

/// 路由拓扑的修订号，由主程序单调递增，用于日志和判断引擎缓存的拓扑是否最新。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TopologyRevision(pub u64);

/// 槽位方向（ADR-0009）。从 Aemeath 的视角命名：
/// 输入方向的槽位提供虚拟输入（应用往里播放），输出方向的槽位提供虚拟输出（OBS 等从里面录）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotDirection {
    In,
    Out,
}

/// 槽位编号，每个方向 1–8（对应设备名 `Aemeath In 1`–`Aemeath In 8` 等）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct SlotNumber(u8);

impl SlotNumber {
    pub const FIRST: u8 = 1;
    /// 每个方向的槽位数（ADR-0009：8 进 8 出）
    pub const LAST: u8 = 8;

    pub fn new(number: u8) -> Result<Self, SlotNumberError> {
        if (Self::FIRST..=Self::LAST).contains(&number) {
            Ok(Self(number))
        } else {
            Err(SlotNumberError(number))
        }
    }

    pub fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for SlotNumber {
    type Error = SlotNumberError;

    fn try_from(number: u8) -> Result<Self, Self::Error> {
        Self::new(number)
    }
}

impl From<SlotNumber> for u8 {
    fn from(number: SlotNumber) -> Self {
        number.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("slot number {0} is out of range {first}..={last}", first = SlotNumber::FIRST, last = SlotNumber::LAST)]
pub struct SlotNumberError(pub u8);

/// 一个槽位：方向加编号。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlotId {
    pub direction: SlotDirection,
    pub number: SlotNumber,
}

impl fmt::Display for SlotId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let direction = match self.direction {
            SlotDirection::In => "In",
            SlotDirection::Out => "Out",
        };
        write!(f, "{direction} {}", self.number.get())
    }
}

/// 外部设备的 Windows 端点 ID（`IMMDevice::GetId` 的结果），按原样传递，不解析。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EndpointId(pub String);

impl EndpointId {
    /// 端点 ID 的长度上限（字节）。实际 ID 约 55 字符，上限只用来拒绝异常输入。
    pub const MAX_LEN: usize = 512;

    pub fn is_well_formed(&self) -> bool {
        !self.0.is_empty() && self.0.len() <= Self::MAX_LEN
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_number_accepts_only_one_to_eight() {
        assert!(SlotNumber::new(0).is_err());
        assert_eq!(SlotNumber::new(1).map(SlotNumber::get), Ok(1));
        assert_eq!(SlotNumber::new(8).map(SlotNumber::get), Ok(8));
        assert!(SlotNumber::new(9).is_err());
    }

    #[test]
    fn slot_number_is_validated_when_deserializing() {
        assert!(serde_json::from_str::<SlotNumber>("3").is_ok());
        assert!(serde_json::from_str::<SlotNumber>("9").is_err());
    }

    #[test]
    fn slot_id_displays_like_the_device_name_suffix() {
        let slot = SlotId {
            direction: SlotDirection::Out,
            number: SlotNumber::new(2).unwrap(),
        };
        assert_eq!(slot.to_string(), "Out 2");
    }
}
