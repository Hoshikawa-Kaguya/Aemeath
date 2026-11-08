//! 设备时钟的读数快照：IO 线程每秒从自己的流读一次 `IAudioClock`，写在这里；主时钟跟随线程（[`crate::follow`]）只读。
//!
//! 以前跟随线程另开一条共享流来读时钟。外部设备默认独占打开后（ADR-0014），第二条流必定打不开，
//! 所以改为从 IO 线程正在使用的流读取，共享模式也统一这样做。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use wasapi::{AudioClock, WasapiError};

/// 读数间隔
pub const READ_PERIOD: Duration = Duration::from_secs(1);

/// 一次读数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockReading {
    /// 流的会话编号：设备重新打开后位置从零开始，跟随线程据此重新建立基准
    pub session: u64,
    /// 设备位置（单位见 `frequency`）
    pub position: u64,
    /// 设备位置每秒增加的量
    pub frequency: u64,
    /// 与位置对应的 QPC 时刻（100 ns）
    pub qpc: i64,
}

/// 一台设备的读数快照。
#[derive(Debug, Clone, Default)]
pub struct ClockTap {
    latest: Arc<Mutex<Option<ClockReading>>>,
}

impl ClockTap {
    pub fn latest(&self) -> Option<ClockReading> {
        *self.latest.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn publish(&self, reading: ClockReading) {
        *self.latest.lock().unwrap_or_else(PoisonError::into_inner) = Some(reading);
    }
}

/// 全局递增的会话编号
static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

/// IO 线程一侧：在会话里每 [`READ_PERIOD`] 读一次时钟。
pub struct ClockReader {
    clock: AudioClock,
    frequency: u64,
    session: u64,
    next: Instant,
}

impl ClockReader {
    pub fn new(client: &wasapi::AudioClient) -> Result<Self, WasapiError> {
        let clock = client.get_audioclock()?;
        let frequency = clock.get_frequency()?;
        Ok(Self {
            clock,
            frequency,
            session: NEXT_SESSION.fetch_add(1, Ordering::Relaxed),
            next: Instant::now() + READ_PERIOD,
        })
    }

    /// 到时间就读一次并写进 `tap`。读失败只跳过这一次（设备真的出错时，流的读写会先报错）。
    pub fn poll(&mut self, tap: &ClockTap) {
        if Instant::now() < self.next {
            return;
        }
        self.next += READ_PERIOD;
        if let Ok((position, qpc)) = self.clock.get_position() {
            tap.publish(ClockReading {
                session: self.session,
                position,
                frequency: self.frequency,
                qpc: i64::try_from(qpc).unwrap_or(i64::MAX),
            });
        }
    }
}
