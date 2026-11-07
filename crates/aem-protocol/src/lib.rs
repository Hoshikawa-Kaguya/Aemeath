//! 引擎与主程序之间的 IPC 消息定义（带版本号的 JSON）。
//!
//! 引擎和主程序后端直接依赖这个 crate，两端共用同一份定义（ADR-0003、ADR-0008）。
//!
//! - [`frame`]：分帧（4 字节小端长度 + UTF-8 JSON），与具体的管道 IO 无关。
//! - [`message`]：两个方向的消息。
//! - [`plan`]：路由计划及其校验。主程序编译计划后、引擎收到计划时都调用同一个 [`plan::RoutePlan::validate`]。
//! - [`ids`] 与 [`units`]：有语义的标识和数值类型，反序列化时就校验取值范围。

pub mod frame;
pub mod ids;
pub mod message;
pub mod plan;
pub mod units;

/// 协议版本。消息格式有任何不兼容的变化都要加一。
///
/// 引擎和主程序总是一起发布，所以握手时要求版本完全相同（见 [`message::AppToEngine::Hello`]）。
pub const PROTOCOL_VERSION: ids::ProtocolVersion = ids::ProtocolVersion(1);
