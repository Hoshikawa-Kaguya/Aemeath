//! 把共享内存里的节点电平推给界面（经 Tauri 的 `Channel` 推送，界面用 `<canvas>` 绘制）。
//!
//! 每 [`PUSH_INTERVAL`] 取走一次峰值，编码成原始字节发出，不经过 JSON：每个节点 12 字节，
//! 依次是节点 ID（u32）、左声道峰值、右声道峰值（f32），全部小端。界面在两次推送之间自己做衰减动画。
//! 窗口关闭（WebView 被销毁）后发送失败，就停止推送，直到新窗口重新订阅。

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use aemeath_control::meters::MeterSource;
use tauri::ipc::{Channel, InvokeResponseBody};
use tracing::debug;

/// 推送间隔（约 30 Hz）
const PUSH_INTERVAL: Duration = Duration::from_millis(33);
const BYTES_PER_NODE: usize = 12;

type Subscriber = Arc<Mutex<Option<Channel<InvokeResponseBody>>>>;

/// 电平推送任务的句柄（Tauri 托管状态）。
/// `Channel` 没有实现 `Debug`，所以这里也不派生
#[derive(Clone)]
pub struct MeterHub {
    subscriber: Subscriber,
}

impl MeterHub {
    /// 启动推送任务：按固定节奏读共享内存推给界面；没有界面订阅时不读。
    pub fn start(memory_name: String) -> Self {
        let subscriber: Subscriber = Arc::new(Mutex::new(None));
        let shared = Arc::clone(&subscriber);
        tauri::async_runtime::spawn(async move {
            let mut source = MeterSource::new(memory_name);
            let mut tick = tokio::time::interval(PUSH_INTERVAL);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                // 没有窗口时不读：峰值留在共享内存里累加，下次订阅后一并取走也无妨
                let Some(channel) = shared.lock().unwrap_or_else(PoisonError::into_inner).clone() else {
                    continue;
                };
                let Some(meters) = source.take() else {
                    continue;
                };
                let mut bytes = Vec::with_capacity(meters.len() * BYTES_PER_NODE);
                for meter in meters {
                    bytes.extend_from_slice(&meter.node.0.to_le_bytes());
                    for peak in meter.peaks {
                        bytes.extend_from_slice(&peak.to_le_bytes());
                    }
                }
                if channel.send(InvokeResponseBody::Raw(bytes)).is_err() {
                    debug!("the meter subscriber is gone");
                    *shared.lock().unwrap_or_else(PoisonError::into_inner) = None;
                }
            }
        });
        Self { subscriber }
    }

    /// 新窗口订阅电平，替换掉之前的订阅。
    pub fn subscribe(&self, channel: Channel<InvokeResponseBody>) {
        *self.subscriber.lock().unwrap_or_else(PoisonError::into_inner) = Some(channel);
    }
}
