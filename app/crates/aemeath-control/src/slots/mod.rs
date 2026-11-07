//! 槽位的挂载维护与首次注册（ADR-0007、ADR-0009）。
//!
//! - [`assess`]：判断一个槽位是否正常挂载（纯函数）。
//! - [`endpoints`]：槽位对应的音频端点（专用 COM 线程）。
//! - [`maintainer`]：挂载维护的后台任务。
//! - [`registration`]：首次注册全部槽位。

pub mod assess;
pub mod endpoints;
pub mod maintainer;
pub mod registration;

pub use maintainer::{MaintainerConfig, MaintainerTiming, SlotHealth, SlotMaintainer, SlotStatus};
