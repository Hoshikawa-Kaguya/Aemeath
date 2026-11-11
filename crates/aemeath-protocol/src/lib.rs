//! 引擎与主程序之间的 IPC 消息定义（带版本号的 JSON）。
//!
//! 引擎和主程序后端直接依赖这个 crate，两端共用同一份定义。
//!
//! - [`frame`]：分帧（4 字节小端长度 + UTF-8 JSON），与具体的管道 IO 无关。
//! - [`message`]：两个方向的消息。
//! - [`topology`]：路由拓扑、节点类型与连线规则。主程序编译拓扑后、引擎收到拓扑时都调用同一个
//!   [`topology::RouteTopology::validate`]；工作台拖线时调用同一套规则 [`topology::check_route`]。
//! - [`graph`]：可达性与拓扑排序，主程序、拓扑校验和引擎混音共用。
//! - [`meters`]：电平共享内存的布局，以及写入方和读取方。
//! - [`ids`] 与 [`units`]：有语义的标识和数值类型，反序列化时就校验取值范围。

pub mod frame;
pub mod graph;
pub mod ids;
pub mod message;
pub mod meters;
pub mod topology;
pub mod units;

/// 协议版本。消息格式有任何不兼容的变化都要加一。
///
/// 引擎和主程序总是一起发布，所以握手时要求版本完全相同（见 [`message::AppToEngine::Hello`]）。
/// 2：通道条带门限，外部设备节点带设备设置。
/// 3：新增应用源、外部插入、录音节点及其状态和命令。
/// 4：重采样算法改为算法加重采样预设。
pub const PROTOCOL_VERSION: ids::ProtocolVersion = ids::ProtocolVersion(4);
