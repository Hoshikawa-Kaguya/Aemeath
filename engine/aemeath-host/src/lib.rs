//! 引擎进程：组装引擎与 USB/IP 服务端，在命名管道上与主程序通信，写日志，缓存最近一份路由拓扑
//! 并在启动时先恢复出声（ADR-0003、ADR-0008）。
//!
//! - [`config`]：命令行参数、数据目录、管道名、电平共享内存名。
//! - [`logging`]：按天轮转的非阻塞文件日志与 panic 记录。
//! - [`topology_cache`]：路由拓扑缓存。
//! - [`status`]：状态缓存与事件到 IPC 消息的转换。
//! - [`session`]：一个客户端连接的协议处理。
//! - [`meter_publisher`]：把节点电平转写到共享内存。
//! - [`run`]：主循环。

pub mod config;
pub mod logging;
pub mod meter_publisher;
pub mod session;
pub mod status;
pub mod topology_cache;

mod host;

pub use host::run;
