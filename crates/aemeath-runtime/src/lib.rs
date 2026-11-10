//! 三个进程（引擎、主程序、守护进程）共用的进程设施。
//!
//! - [`config`]：命令行参数（数据目录、USB/IP 端口、管道名、电平共享内存名）。
//! - [`logging`]：按天轮转的非阻塞文件日志与 panic 记录，每个进程一组文件。

pub mod autostart;
pub mod config;
pub mod logging;

/// 程序版本号，来自 `scripts/version.json`（见本 crate 的构建脚本）。不用 Cargo.toml 里的版本。
pub const VERSION: &str = env!("AEMEATH_VERSION");
