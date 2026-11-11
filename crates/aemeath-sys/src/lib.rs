//! Windows API 与 COM 接口的安全封装。
//!
//! 这是全工程唯一允许 `unsafe` 的 crate：其他 crate 只调用这里提供的安全接口。
//! 每个 `unsafe` 块都必须写 `// SAFETY:` 注释，说明为什么满足调用约定。

pub mod app_routing;
pub mod audio_endpoints;
pub mod audio_sessions;
pub mod clock;
pub mod com;
pub mod devices;
pub mod elevation;
pub mod input_hooks;
pub mod pipe;
pub mod policy_config;
pub mod process;
pub mod registry;
mod security;
pub mod service;
pub mod shared_memory;
pub mod single_instance;
pub mod task_dialog;
pub mod thread_priority;
pub mod timer;
mod token;
pub mod ui_language;
