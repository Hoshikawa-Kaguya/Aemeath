//! 高精度可等待定时器（`CREATE_WAITABLE_TIMER_HIGH_RESOLUTION`，Windows 10 1803 起）。
//!
//! 调度线程如果用普通 `sleep` 等待，会受 1 ms 系统定时精度限制，实测偶尔滞后到 2.7 ms；
//! 高精度定时器不依赖 `timeBeginPeriod`，精度约 0.5 ms 以内。

use std::io;

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, INFINITE, SetWaitableTimer, TIMER_ALL_ACCESS,
    WaitForSingleObject,
};
use windows::core::PCWSTR;

/// 一个只在创建它的线程里使用的定时器。
///
/// 所有权：句柄由本对象独占，析构时关闭。没有实现 `Send`（`HANDLE` 含裸指针），
/// 所以只能在创建它的线程里使用，调度线程启动后自行创建即可。
pub struct HighResolutionTimer {
    handle: HANDLE,
}

impl HighResolutionTimer {
    /// 创建匿名的高精度定时器；系统不支持（Windows 10 1803 之前）时报错。
    pub fn new() -> io::Result<Self> {
        // SAFETY: 不传安全属性和名称（匿名定时器）；返回的句柄由本对象独占并在 Drop 中关闭。
        let handle = unsafe {
            CreateWaitableTimerExW(
                None,
                PCWSTR::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS.0,
            )
        }?;
        Ok(Self { handle })
    }

    /// 阻塞当前线程约 `hns`（100 ns 单位）；`hns <= 0` 时立即返回。
    pub fn sleep(&self, hns: i64) -> io::Result<()> {
        if hns <= 0 {
            return Ok(());
        }
        // 负值表示相对时间
        let due = -hns;
        // SAFETY: 句柄有效（由本对象持有）；`due` 是有效的 i64 指针，调用期间一直存活；不使用完成例程。
        unsafe { SetWaitableTimer(self.handle, &due, 0, None, None, false) }?;
        // SAFETY: 句柄有效；无限等待一个刚设置的一次性定时器，它一定会触发。
        let result = unsafe { WaitForSingleObject(self.handle, INFINITE) };
        if result == WAIT_OBJECT_0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

impl Drop for HighResolutionTimer {
    fn drop(&mut self) {
        // SAFETY: 句柄由本对象独占，只在这里关闭一次。
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::now_hns;

    /// 0.5 ms 的等待应在 0.5–3 ms 内返回（CI 机器负载高时放宽上限）
    #[test]
    fn sleeps_for_about_the_requested_time() {
        let timer = HighResolutionTimer::new().unwrap();
        let start = now_hns();
        timer.sleep(5_000).unwrap();
        let elapsed = now_hns() - start;
        assert!(elapsed >= 4_500, "elapsed {elapsed}");
        assert!(elapsed < 30_000, "elapsed {elapsed}");
    }
}
