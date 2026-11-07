//! 把引擎的节点电平转写到共享内存，供主程序读取（布局见 `aemeath_protocol::meters`）。
//!
//! 实时线程只写进程内的 [`MeterBank`]，不碰共享内存；这里在非实时任务中每 [`PUBLISH_INTERVAL`] 取走一次峰值，
//! 累加进共享内存。主程序按自己的节奏取走，读得再慢也不会漏掉峰值。

use std::sync::Arc;
use std::time::Duration;

use aemeath_engine::meters::MeterBank;
use aemeath_protocol::meters::{METER_CHANNELS, METER_MEMORY_WORDS, MeterWriter};
use aemeath_sys::shared_memory::SharedMemory;
use tokio::sync::watch;

/// 转写间隔：比界面的刷新间隔（约 33 ms）短，界面每次都能拿到新数据
pub const PUBLISH_INTERVAL: Duration = Duration::from_millis(20);

// 共享内存布局的声道数必须与引擎内部的声道数一致
const _: () = assert!(METER_CHANNELS == aemeath_engine::STEREO);

/// 当前拓扑的电平组。每次换拓扑，引擎都会换一组新的。
pub type MeterBanks = watch::Receiver<Option<Arc<MeterBank>>>;

/// 创建共享内存并启动转写任务。共享内存已存在时（主程序还开着上一个引擎的映射）直接沿用。
pub fn spawn(name: &str, mut banks: MeterBanks) -> std::io::Result<tokio::task::JoinHandle<()>> {
    let memory = SharedMemory::create(name, METER_MEMORY_WORDS)?;
    tracing::info!(name, reused = memory.existed(), "meter memory ready");
    Ok(tokio::spawn(async move {
        let writer = match MeterWriter::new(memory.words()) {
            Ok(writer) => writer,
            Err(error) => {
                tracing::error!(%error, "cannot write meters");
                return;
            }
        };
        let mut bank = banks.borrow_and_update().clone();
        writer.set_nodes(bank.as_deref().map(MeterBank::node_ids).unwrap_or_default());
        let mut tick = tokio::time::interval(PUBLISH_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                changed = banks.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    bank = banks.borrow_and_update().clone();
                    writer.set_nodes(bank.as_deref().map(MeterBank::node_ids).unwrap_or_default());
                }
                _ = tick.tick() => {
                    let Some(bank) = bank.as_deref() else {
                        continue;
                    };
                    for index in 0..bank.node_ids().len() {
                        if let Some(peaks) = bank.take(index) {
                            writer.record(index, peaks);
                        }
                    }
                }
            }
        }
    }))
}
