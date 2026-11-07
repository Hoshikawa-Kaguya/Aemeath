//! Windows API 与 COM 接口的安全封装。
//!
//! 这是全工程唯一允许 `unsafe` 的 crate（ADR-0008）：其他 crate 只调用这里提供的安全接口。
//! 每个 `unsafe` 块都必须写 `// SAFETY:` 注释，说明为什么满足调用约定。

pub mod clock;
pub mod timer;
