//! 标识类型。用 newtype 而不是裸数字或字符串，避免把一种 ID 误当成另一种。

use std::fmt;
use std::path::Path;

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

/// 槽位方向。从 Aemeath 的视角命名：
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
    /// 编号从 1 开始，与 Windows 里的设备名一致
    pub const FIRST: u8 = 1;
    /// 每个方向的槽位数（8 进 8 出）
    pub const LAST: u8 = 8;

    /// 校验编号在 [`Self::FIRST`]–[`Self::LAST`] 之内。
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

    /// 一个方向的全部槽位编号，从小到大。
    pub fn all() -> impl Iterator<Item = Self> {
        (Self::FIRST..=Self::LAST).map(Self)
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

impl SlotId {
    /// 某个方向的全部槽位，编号从小到大。
    pub fn all(direction: SlotDirection) -> impl Iterator<Item = Self> {
        SlotNumber::all().map(move |number| Self { direction, number })
    }
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

    /// 非空且不超长。只做粗查，真正能不能打开要交给 Windows。
    pub fn is_well_formed(&self) -> bool {
        !self.0.is_empty() && self.0.len() <= Self::MAX_LEN
    }
}

/// 应用源绑定的程序：可执行文件的绝对路径。反序列化时就校验。
///
/// Windows 的路径不区分大小写，比较"是不是同一个程序"用 [`Self::same_program`]，派生的 `Eq` 只比较原文。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProgramPath(String);

impl ProgramPath {
    /// Windows 路径的长度上限（字符数），只用来拒绝异常输入
    pub const MAX_CHARS: usize = 32_767;
    const EXTENSION: &str = "exe";

    /// 校验后包装：必须是指向 `.exe` 的绝对路径。不检查文件是否存在，程序缺失是运行时的状态。
    pub fn new(path: &str) -> Result<Self, InvalidProgramPath> {
        let typed = Path::new(path);
        let is_exe = typed
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case(Self::EXTENSION));
        if typed.is_absolute() && is_exe && path.chars().count() <= Self::MAX_CHARS {
            Ok(Self(path.to_owned()))
        } else {
            Err(InvalidProgramPath)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn as_path(&self) -> &Path {
        Path::new(&self.0)
    }

    /// 文件名，例如 `chrome.exe`
    pub fn file_name(&self) -> &str {
        self.as_path()
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&self.0)
    }

    /// 是否指向同一个程序（不区分大小写）。
    pub fn same_program(&self, other: &Self) -> bool {
        self.0.to_lowercase() == other.0.to_lowercase()
    }
}

impl TryFrom<String> for ProgramPath {
    type Error = InvalidProgramPath;

    fn try_from(path: String) -> Result<Self, Self::Error> {
        Self::new(&path)
    }
}

impl From<ProgramPath> for String {
    fn from(path: ProgramPath) -> Self {
        path.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("a program path must be an absolute path to an .exe file")]
pub struct InvalidProgramPath;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_paths_are_absolute_exe_paths() {
        let chrome = ProgramPath::new(r"C:\Program Files\Google\Chrome.EXE").unwrap();
        assert_eq!(chrome.file_name(), "Chrome.EXE");
        assert!(chrome.same_program(&ProgramPath::new(r"c:\program files\google\chrome.exe").unwrap()));
        assert_eq!(ProgramPath::new("chrome.exe"), Err(InvalidProgramPath));
        assert_eq!(ProgramPath::new(r"C:\Windows\notepad.txt"), Err(InvalidProgramPath));
        assert!(serde_json::from_str::<ProgramPath>(r#""relative\\app.exe""#).is_err());
    }

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
