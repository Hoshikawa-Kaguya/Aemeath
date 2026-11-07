//! USB/IP 服务端与 UAC2 设备模型：16 个固定槽位（ADR-0009），声明全部格式（ADR-0010）。
//!
//! 不含 DSP，也不含时间线和调度：等时 URB 由引擎的主时钟调度线程按时间线完成（S4）。
//!
//! - [`wire`]：USB/IP 报文解析与编码，纯字节处理，带全部上限校验（对外的攻击面）。
//! - [`identity`]：槽位的固定 USB 身份。
//! - [`descriptors`]：UAC2 描述符。
//! - [`function`]：控制请求处理与流格式。
//! - [`stream`]：流格式与等时包的节拍。
//! - [`server`]：TCP 服务端，把 URB 以事件交给引擎。

pub mod descriptors;
pub mod function;
pub mod identity;
pub mod server;
pub mod stream;
pub mod wire;
