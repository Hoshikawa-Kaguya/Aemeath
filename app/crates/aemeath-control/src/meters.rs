//! 读取引擎写在共享内存里的节点电平（布局见 `aemeath_protocol::meters`）。
//!
//! 由界面按自己的刷新节奏调用（约 30 Hz）。引擎还没启动时共享内存不存在，下次读取时再尝试打开；
//! 打开后一直持有：引擎重启时会沿用这块内存，主程序这边不中断。

use aemeath_protocol::meters::{METER_MEMORY_WORDS, MeterReader, NodeMeter};
use aemeath_sys::shared_memory::SharedMemory;
use tracing::{debug, warn};

/// 引擎电平共享内存的读取端，按名称延迟打开。
#[derive(Debug)]
pub struct MeterSource {
    name: String,
    memory: Option<SharedMemory>,
}

impl MeterSource {
    pub fn new(name: String) -> Self {
        Self { name, memory: None }
    }

    /// 取走全部节点自上次读取以来的峰值。引擎没有运行、或者节点表正在变化时返回 None。
    pub fn take(&mut self) -> Option<Vec<NodeMeter>> {
        if self.memory.is_none() {
            match SharedMemory::open(&self.name, METER_MEMORY_WORDS) {
                Ok(memory) => {
                    debug!(name = %self.name, "meter memory opened");
                    self.memory = Some(memory);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
                Err(error) => {
                    warn!(%error, "cannot open the meter memory");
                    return None;
                }
            }
        }
        let memory = self.memory.as_ref()?;
        MeterReader::new(memory.words()).ok()?.take()
    }
}
