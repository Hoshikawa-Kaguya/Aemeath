//! 两个方向的消息。
//!
//! 连接建立后，主程序先发 [`AppToEngine::Hello`]；引擎回 [`EngineToApp::Welcome`] 后才处理其他命令。
//! 命令都带 [`RequestId`]，引擎用 [`EngineToApp::Ack`] 或 [`EngineToApp::Rejected`] 应答；
//! 设备状态等事件由引擎主动推送，不需要应答。电平和录音数据不走这里，走共享内存（ADR-0003）。

use serde::{Deserialize, Serialize};

use crate::ids::{NodeId, PlanRevision, ProtocolVersion, RequestId, SlotId};
use crate::plan::RoutePlan;
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
    /// 整体替换路由计划。引擎校验通过后原子替换，并缓存到磁盘。
    ApplyPlan { request: RequestId, plan: RoutePlan },
    /// 心跳。主程序据此判断引擎是否假死（ADR-0008）。
    Ping { request: RequestId },
    /// 用户从托盘退出时，请引擎停止所有流、关闭 USB/IP 服务端后退出。
    Shutdown { request: RequestId },
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
        /// 引擎启动时从缓存恢复的计划修订号；为 None 表示没有缓存，引擎处于静音的空计划
        restored_plan: Option<PlanRevision>,
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
}

/// 命令被拒绝的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    /// 还没有完成握手
    NotHandshaken,
    /// 路由计划没有通过校验（详见 detail）
    InvalidPlan,
    /// 计划的修订号不比当前的新
    StalePlan,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfflineReason {
    /// 等待设备：端点不存在，或者打不开
    WaitingForDevice,
    /// 端点被其他程序独占（`AUDCLNT_E_DEVICE_IN_USE`）
    InUseByOtherApp,
    /// 端点打开了，但不再送出或接收声音
    NoSignal,
}

/// 设备格式：Windows 为端点确定的采样率和样本格式（ADR-0010）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceFormat {
    pub sample_rate: SampleRate,
    pub sample_format: SampleFormat,
    pub channels: u16,
}

/// 样本格式。24 位整数分为紧凑的 3 字节和放在 32 位容器里两种，USB 音频和 WASAPI 都会用到。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleFormat {
    Int16,
    Int24,
    Int24In32,
    Int32,
    Float32,
    Float64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{SlotDirection, SlotNumber};

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
    fn unknown_message_types_are_rejected() {
        assert!(serde_json::from_str::<AppToEngine>(r#"{"type":"format_disk","request":1}"#).is_err());
    }

    #[test]
    fn unknown_fields_are_rejected() {
        assert!(serde_json::from_str::<AppToEngine>(r#"{"type":"ping","request":1,"extra":0}"#).is_err());
    }
}
