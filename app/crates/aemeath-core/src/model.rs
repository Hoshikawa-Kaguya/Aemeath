//! 领域模型（术语见 GLOSSARY.md）：工作台上的节点与连线，以及引擎设置。
//!
//! 这些类型就是配置文件的内容（[`crate::store`]），也是编译路由拓扑的来源（[`crate::compile`]）。
//! 只描述"用户搭建了什么"，不含运行时状态（设备是否在线、槽位是否已挂载等由控制面另行跟踪）。

use aemeath_protocol::ids::{EndpointId, NodeId};
use aemeath_protocol::topology::{self, DeviceSettings, Resampler, Strip};
use aemeath_protocol::units::{GainDb, SampleRate};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 节点名：用户可以随时修改，不影响设备名（GLOSSARY：节点名）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct NodeName(String);

impl NodeName {
    /// 节点名的长度上限（字符数）
    pub const MAX_CHARS: usize = 64;

    /// 去掉首尾空白后不能为空，也不能超过 [`Self::MAX_CHARS`] 个字符。
    pub fn new(name: &str) -> Result<Self, InvalidName> {
        let trimmed = name.trim();
        if trimmed.is_empty() || trimmed.chars().count() > Self::MAX_CHARS {
            Err(InvalidName)
        } else {
            Ok(Self(trimmed.to_owned()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for NodeName {
    type Error = InvalidName;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        Self::new(&name)
    }
}

impl From<NodeName> for String {
    fn from(name: NodeName) -> Self {
        name.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("node names must be 1 to {max} characters", max = NodeName::MAX_CHARS)]
pub struct InvalidName;

/// 外部设备：Windows 端点 ID，加上设备名。端点 ID 找不到时按设备名加方向做后备匹配（选型报告 6.5 节）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceRef {
    pub endpoint: EndpointId,
    /// 设备名（Windows 显示的名称），只用于后备匹配和显示
    pub name: String,
}

/// 工作台里的节点类型：与路由拓扑共用同一个定义，外部设备带上设备名。
pub type NodeKind = topology::NodeKind<DeviceRef>;

/// 节点在工作台上的位置（工作台坐标，与缩放无关）。
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Position {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    pub id: NodeId,
    pub name: NodeName,
    pub kind: NodeKind,
    #[serde(default)]
    pub strip: Strip,
    /// 外部设备节点的设备设置（ADR-0014）；其他节点为 None。外部设备节点缺少它时按默认值（独占）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub io: Option<DeviceSettings>,
    #[serde(default)]
    pub position: Position,
}

impl Node {
    /// 外部设备节点实际使用的设备设置；其他节点为 None
    pub fn device_settings(&self) -> Option<DeviceSettings> {
        self.kind.is_external().then(|| self.io.unwrap_or_default())
    }
}

/// 连线标签在连线上的位置（0 起点、1 终点），用户可以拖动以免标签重叠。只是显示偏好。
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(from = "f32", into = "f32")]
pub struct LabelPosition(f32);

impl LabelPosition {
    pub const MIN: f32 = 0.1;
    pub const MAX: f32 = 0.9;
    pub const MIDDLE: Self = Self(0.5);

    /// 超出范围的值被限制到 [`Self::MIN`]–[`Self::MAX`]；非有限值视为中间。
    pub fn new(position: f32) -> Self {
        if position.is_finite() {
            Self(position.clamp(Self::MIN, Self::MAX))
        } else {
            Self::MIDDLE
        }
    }

    pub fn get(self) -> f32 {
        self.0
    }
}

impl Default for LabelPosition {
    fn default() -> Self {
        Self::MIDDLE
    }
}

impl From<f32> for LabelPosition {
    fn from(position: f32) -> Self {
        Self::new(position)
    }
}

impl From<LabelPosition> for f32 {
    fn from(position: LabelPosition) -> Self {
        position.0
    }
}

/// 连线：从 `from` 到 `to` 的有向发送。关闭的连线保留配置，不同于删除（GLOSSARY：连线）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub from: NodeId,
    pub to: NodeId,
    pub gain: GainDb,
    pub enabled: bool,
    #[serde(default)]
    pub label_position: LabelPosition,
}

/// 引擎设置。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineSettings {
    /// 引擎采样率（GLOSSARY）
    pub sample_rate: SampleRate,
    /// 全局默认的重采样算法；外部设备节点可以在设备设置里单独指定
    #[serde(default)]
    pub resampler: Resampler,
    /// 主时钟：外部设备节点；为 None 时跟随系统时钟
    #[serde(default)]
    pub clock_master: Option<NodeId>,
}

impl Default for EngineSettings {
    fn default() -> Self {
        Self {
            sample_rate: SampleRate::HZ_48000,
            resampler: Resampler::default(),
            clock_master: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_names_are_trimmed_and_bounded() {
        assert_eq!(NodeName::new("  游戏  ").unwrap().as_str(), "游戏");
        assert_eq!(NodeName::new("   "), Err(InvalidName));
        assert!(NodeName::new(&"音".repeat(NodeName::MAX_CHARS)).is_ok());
        assert_eq!(NodeName::new(&"音".repeat(NodeName::MAX_CHARS + 1)), Err(InvalidName));
        assert!(serde_json::from_str::<NodeName>("\"\"").is_err());
    }

    #[test]
    fn label_positions_are_clamped() {
        assert_eq!(LabelPosition::new(0.0).get(), LabelPosition::MIN);
        assert_eq!(LabelPosition::new(2.0).get(), LabelPosition::MAX);
        assert_eq!(LabelPosition::new(f32::NAN), LabelPosition::MIDDLE);
        assert_eq!(
            serde_json::from_str::<LabelPosition>("0.95").unwrap().get(),
            LabelPosition::MAX
        );
    }

    #[test]
    fn node_kind_has_a_stable_format() {
        let kind = NodeKind::ExternalInput {
            device: DeviceRef {
                endpoint: EndpointId("{0.0.1.00000000}.{mic}".to_owned()),
                name: "Rear Mic".to_owned(),
            },
        };
        assert_eq!(
            serde_json::to_string(&kind).unwrap(),
            r#"{"type":"external_input","device":{"endpoint":"{0.0.1.00000000}.{mic}","name":"Rear Mic"}}"#
        );
    }
}
