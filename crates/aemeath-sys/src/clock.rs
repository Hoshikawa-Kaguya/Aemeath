//! 系统高精度时钟（QPC）。
//!
//! 时间一律换算成 100 ns 单位，与 WASAPI 的 `u64QPCPosition` 相同，引擎、设备流和 USB/IP 时间线的时间戳可以直接比较。

use std::sync::OnceLock;

use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::SystemInformation::GetLocalTime;

/// 本地时间（用于录音文件名）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalDateTime {
    pub year: u16,
    pub month: u16,
    pub day: u16,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
}

/// 当前的本地时间，用于录音文件名等给用户看的时间。
pub fn local_date_time() -> LocalDateTime {
    // SAFETY: GetLocalTime 没有前置条件，总是成功
    let now = unsafe { GetLocalTime() };
    LocalDateTime {
        year: now.wYear,
        month: now.wMonth,
        day: now.wDay,
        hour: now.wHour,
        minute: now.wMinute,
        second: now.wSecond,
    }
}

/// 每秒的 100 ns 单位数
pub const HNS_PER_SECOND: i64 = 10_000_000;

/// QPC 频率在系统启动后固定不变，只读一次。
fn frequency() -> i64 {
    static FREQUENCY: OnceLock<i64> = OnceLock::new();
    *FREQUENCY.get_or_init(|| {
        let mut frequency = 0i64;
        // SAFETY: 传入的是有效的 i64 可写指针；该函数在 Windows XP 之后的系统上总是成功。
        let result = unsafe { QueryPerformanceFrequency(&mut frequency) };
        if result.is_err() || frequency <= 0 {
            // 不会发生；退回 1 MHz 只是为了不除以零
            1_000_000
        } else {
            frequency
        }
    })
}

/// 当前时刻，单位 100 ns（QPC 起点，单调递增）。
pub fn now_hns() -> i64 {
    let mut counter = 0i64;
    // SAFETY: 传入的是有效的 i64 可写指针；该函数在 Windows XP 之后的系统上总是成功。
    let _ = unsafe { QueryPerformanceCounter(&mut counter) };
    let hns = i128::from(counter) * i128::from(HNS_PER_SECOND) / i128::from(frequency());
    i64::try_from(hns).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_is_monotonic_and_roughly_real_time() {
        let start = now_hns();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let elapsed = now_hns() - start;
        assert!(elapsed >= 190_000, "elapsed {elapsed}");
        assert!(elapsed < 2 * HNS_PER_SECOND, "elapsed {elapsed}");
    }
}
