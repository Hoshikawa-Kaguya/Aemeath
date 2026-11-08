//! 两个方向的消息。
//!
//! 连接建立后，主程序先发 [`AppToEngine::Hello`]；引擎回 [`EngineToApp::Welcome`] 后才处理其他命令。
//! 命令都带 [`RequestId`]，引擎用 [`EngineToApp::Ack`] 或 [`EngineToApp::Rejected`] 应答；
//! 设备状态等事件由引擎主动推送，不需要应答。电平和录音数据不走这里，走共享内存（ADR-0003）。

use serde::{Deserialize, Serialize};

use crate::ids::{EndpointId, NodeId, ProtocolVersion, RequestId, SlotId, TopologyRevision};
use crate::topology::RouteTopology;
use crate::units::SampleRate;

/// 主程序 → 引擎。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppToEngine {
    /// 握手。协议版本必须与引擎完全相同。
    Hello {
        protocol: ProtocolVersion,
        /// 主程序版本，只用于日志
        app_version: String,
    },
    /// 整体替换路由拓扑。引擎校验通过后原子替换，并缓存到磁盘。
    ApplyTopology {
        request: RequestId,
        topology: RouteTopology,
    },
    /// 心跳。主程序据此判断引擎是否假死（ADR-0008）。
    Ping { request: RequestId },
    /// 用户从托盘退出时，请引擎停止所有流、关闭 USB/IP 服务端后退出。
    Shutdown { request: RequestId },
    /// 查询外部设备的能力（独占时支持的格式、共享格式、周期），引擎用 [`EngineToApp::DeviceCapabilities`] 应答。
    /// 设备正被 Aemeath 打开时返回打开前探测的结果。
    ProbeDevice {
        request: RequestId,
        endpoint: EndpointId,
        direction: EndpointDirection,
    },
    /// 立即重新检测离线的外部设备（选型报告 6.5 节的手动检测）；`node` 为 None 表示全部。
    Redetect { request: RequestId, node: Option<NodeId> },
}

/// 外部设备的方向，从 Aemeath 的视角命名：输入是录音端点，输出是播放端点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointDirection {
    Input,
    Output,
}

/// 引擎 → 主程序。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EngineToApp {
    /// 握手成功。
    Welcome {
        protocol: ProtocolVersion,
        /// 引擎版本，只用于日志
        engine_version: String,
        /// 引擎启动时从缓存恢复的拓扑修订号；为 None 表示没有缓存，引擎处于静音的空拓扑
        restored_topology: Option<TopologyRevision>,
    },
    /// 握手失败：协议版本不同。引擎随后关闭连接。
    HelloRejected { engine_protocol: ProtocolVersion },
    /// 命令已执行。
    Ack { request: RequestId },
    /// 命令被拒绝。
    Rejected {
        request: RequestId,
        reason: RejectReason,
        /// 给日志看的详细说明（英文），不显示给用户
        detail: String,
    },
    /// 心跳应答。
    Pong { request: RequestId },
    /// IO 节点所代表端点的状态发生变化。
    EndpointStatus { node: NodeId, status: EndpointStatus },
    /// USB/IP 服务端上某个槽位的连接状态发生变化（ADR-0007：挂载成功以服务端实际收到 import 为准）。
    SlotConnection { slot: SlotId, connected: bool },
    /// 主机为槽位选定的流格式（设备页显示）；为 None 表示流已停止。
    SlotFormat { slot: SlotId, format: Option<DeviceFormat> },
    /// [`AppToEngine::ProbeDevice`] 的应答；设备不存在或探测失败时为 None。
    DeviceCapabilities {
        request: RequestId,
        capabilities: Option<DeviceCapabilities>,
    },
}

/// 外部设备的能力（ADR-0014：独占时的格式和缓冲由用户在其中选择）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceCapabilities {
    /// 独占模式下设备接受的格式（声道数都是设备的声道数）
    pub exclusive_formats: Vec<DeviceFormat>,
    /// 共享模式的格式（Windows 确定）
    pub shared_format: Option<DeviceFormat>,
    /// 设备的默认周期（µs）
    pub default_period_us: u32,
    /// 设备的最小周期（µs）
    pub minimum_period_us: u32,
}

impl DeviceCapabilities {
    /// 推荐的独占格式（ADR-0014）：采样率优先用引擎采样率，其次用共享格式的采样率，再其次取最高的；
    /// 样本格式取位深最高的（32 位整数优先于 32 位浮点，其次是 24 位）。
    pub fn recommended(&self, engine_rate: SampleRate) -> Option<DeviceFormat> {
        let shared_rate = self.shared_format.map(|format| format.sample_rate);
        let has_rate = |rate: SampleRate| self.exclusive_formats.iter().any(|format| format.sample_rate == rate);
        let rate = if has_rate(engine_rate) {
            engine_rate
        } else if let Some(shared) = shared_rate.filter(|rate| has_rate(*rate)) {
            shared
        } else {
            self.exclusive_formats.iter().map(|format| format.sample_rate).max()?
        };
        self.exclusive_formats
            .iter()
            .filter(|format| format.sample_rate == rate)
            .max_by_key(|format| format.sample_format.preference())
            .copied()
    }
}

/// 命令被拒绝的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    /// 还没有完成握手
    NotHandshaken,
    /// 路由拓扑没有通过校验（详见 detail）
    InvalidTopology,
    /// 拓扑的修订号不比当前的新
    StaleTopology,
}

/// 端点的状态（GLOSSARY：离线；选型报告 6.5 节）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum EndpointStatus {
    /// 在线，按设备格式收发
    Online { format: DeviceFormat },
    /// 离线，保留全部配置，原因见 [`OfflineReason`]
    Offline { reason: OfflineReason },
}

/// 离线的原因（GLOSSARY：离线）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfflineReason {
    /// 等待设备：端点不存在，或者打不开
    WaitingForDevice,
    /// 被其他程序占用（`AUDCLNT_E_DEVICE_IN_USE`）：其他程序独占着端点，或者 Aemeath 要独占时它正被其他程序使用
    InUseByOtherApp,
    /// 设备不允许独占（`AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED`）：Windows 声音设置里关掉了独占控制，重试无用
    ExclusiveNotAllowed,
    /// 端点打开了，但不再交付数据（数字静音仍是有信号）
    NoSignal,
}

/// 设备格式：端点实际收发所用的采样率和样本格式（共享时由 Windows 确定，独占时由用户选定；ADR-0010、ADR-0014）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceFormat {
    pub sample_rate: SampleRate,
    pub sample_format: SampleFormat,
    pub channels: u16,
}

/// 样本格式。24 位整数分为紧凑的 3 字节和放在 32 位容器里两种，USB 音频和 WASAPI 都会用到。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleFormat {
    Int16,
    Int24,
    Int24In32,
    Int32,
    Float32,
    Float64,
}

impl SampleFormat {
    /// 选推荐格式时的优先级，越大越好：位深越高越好，32 位整数优先于 32 位浮点（整数直通到 DAC，不经浮点换算）
    fn preference(self) -> u8 {
        match self {
            Self::Int16 => 0,
            Self::Int24 => 1,
            Self::Int24In32 => 2,
            Self::Float64 => 3,
            Self::Float32 => 4,
            Self::Int32 => 5,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{SlotDirection, SlotNumber};

    fn format(hz: u32, sample_format: SampleFormat) -> DeviceFormat {
        DeviceFormat {
            sample_rate: SampleRate::new(hz).unwrap(),
            sample_format,
            channels: 2,
        }
    }

    fn capabilities(formats: Vec<DeviceFormat>, shared_hz: u32) -> DeviceCapabilities {
        DeviceCapabilities {
            exclusive_formats: formats,
            shared_format: Some(format(shared_hz, SampleFormat::Float32)),
            default_period_us: 10_000,
            minimum_period_us: 3_000,
        }
    }

    #[test]
    fn recommends_the_engine_rate_and_the_deepest_format() {
        let device = capabilities(
            vec![
                format(44_100, SampleFormat::Int32),
                format(48_000, SampleFormat::Int16),
                format(48_000, SampleFormat::Int24In32),
                format(48_000, SampleFormat::Int32),
                format(48_000, SampleFormat::Float32),
            ],
            44_100,
        );
        assert_eq!(
            device.recommended(SampleRate::HZ_48000),
            Some(format(48_000, SampleFormat::Int32))
        );
    }

    #[test]
    fn falls_back_to_the_shared_rate_then_the_highest_rate() {
        let device = capabilities(
            vec![format(44_100, SampleFormat::Int24), format(96_000, SampleFormat::Int16)],
            44_100,
        );
        assert_eq!(
            device.recommended(SampleRate::HZ_48000),
            Some(format(44_100, SampleFormat::Int24))
        );
        let device = capabilities(vec![format(96_000, SampleFormat::Int16)], 44_100);
        assert_eq!(
            device.recommended(SampleRate::HZ_48000),
            Some(format(96_000, SampleFormat::Int16))
        );
        assert_eq!(capabilities(Vec::new(), 48_000).recommended(SampleRate::HZ_48000), None);
    }

    #[test]
    fn probe_and_redetect_have_a_stable_wire_format() {
        let probe = AppToEngine::ProbeDevice {
            request: RequestId(3),
            endpoint: EndpointId("{lg}".to_owned()),
            direction: EndpointDirection::Output,
        };
        assert_eq!(
            serde_json::to_string(&probe).unwrap(),
            r#"{"type":"probe_device","request":3,"endpoint":"{lg}","direction":"output"}"#
        );
        let redetect = AppToEngine::Redetect {
            request: RequestId(4),
            node: None,
        };
        assert_eq!(
            serde_json::to_string(&redetect).unwrap(),
            r#"{"type":"redetect","request":4,"node":null}"#
        );
    }

    #[test]
    fn hello_has_a_stable_wire_format() {
        let hello = AppToEngine::Hello {
            protocol: ProtocolVersion(1),
            app_version: "0.1.0".to_owned(),
        };
        assert_eq!(
            serde_json::to_string(&hello).unwrap(),
            r#"{"type":"hello","protocol":1,"app_version":"0.1.0"}"#
        );
    }

    #[test]
    fn endpoint_status_has_a_stable_wire_format() {
        let message = EngineToApp::EndpointStatus {
            node: NodeId(3),
            status: EndpointStatus::Offline {
                reason: OfflineReason::InUseByOtherApp,
            },
        };
        assert_eq!(
            serde_json::to_string(&message).unwrap(),
            r#"{"type":"endpoint_status","node":3,"status":{"state":"offline","reason":"in_use_by_other_app"}}"#
        );
    }

    #[test]
    fn slot_connection_round_trips() {
        let message = EngineToApp::SlotConnection {
            slot: SlotId {
                direction: SlotDirection::Out,
                number: SlotNumber::new(8).unwrap(),
            },
            connected: true,
        };
        let json = serde_json::to_string(&message).unwrap();
        assert_eq!(
            json,
            r#"{"type":"slot_connection","slot":{"direction":"out","number":8},"connected":true}"#
        );
        assert_eq!(serde_json::from_str::<EngineToApp>(&json).unwrap(), message);
    }

    #[test]
    fn slot_format_has_a_stable_wire_format() {
        let message = EngineToApp::SlotFormat {
            slot: SlotId {
                direction: SlotDirection::In,
                number: SlotNumber::new(1).unwrap(),
            },
            format: Some(DeviceFormat {
                sample_rate: SampleRate::HZ_48000,
                sample_format: SampleFormat::Int32,
                channels: 2,
            }),
        };
        assert_eq!(
            serde_json::to_string(&message).unwrap(),
            r#"{"type":"slot_format","slot":{"direction":"in","number":1},"format":{"sample_rate":48000,"sample_format":"int32","channels":2}}"#
        );
    }

    #[test]
    fn unknown_message_types_are_rejected() {
        assert!(serde_json::from_str::<AppToEngine>(r#"{"type":"format_disk","request":1}"#).is_err());
    }

    #[test]
    fn unknown_fields_are_rejected() {
        assert!(serde_json::from_str::<AppToEngine>(r#"{"type":"ping","request":1,"extra":0}"#).is_err());
    }
}
