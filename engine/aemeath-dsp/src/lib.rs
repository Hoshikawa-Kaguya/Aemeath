//! 纯计算的音频处理，便于离线测试。
//!
//! - [`codec`]：样本格式与 f32 的互相转换。
//! - [`gain`]：dB 换算与平滑的增益。
//! - [`gate`]：输入节点的门限。
//! - [`resample`]：4 档重采样与时钟漂移的 PI 控制器。

pub mod codec;
pub mod gain;
pub mod gate;
pub mod resample;

/// 引擎内部一律是立体声
pub const STEREO: usize = 2;
