//! 控制面：槽位注册与挂载维护（ADR-0007）、`usbip.exe` 命令行封装、设备枚举、与引擎的 IPC。
//!
//! - [`usbip_cli`]：`usbip.exe` 命令行封装与输出解析。
//! - [`engine_client`]：与引擎的 IPC 客户端（握手、心跳、下发拓扑、断线重连）。
//! - [`meters`]：读取共享内存中的节点电平。
//! - [`slots`]：槽位的挂载维护（ADR-0007）与首次注册（ADR-0009）。
//! - [`audio_policy`]：应用会话与按应用指派、Windows 默认设备。

pub mod audio_policy;
pub mod engine_client;
pub mod hotkeys;
pub mod meters;
pub mod slots;
pub mod usbip_cli;
