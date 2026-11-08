//! 音频相关线程的优先级。
//!
//! - [`promote_current_thread`]：真正处理音频的线程（调度线程、设备 IO 线程）加入 MMCSS 的 "Pro Audio" 任务（High 类别）。
//!   默认的 "Audio" 任务只是 Medium 类别。
//! - [`raise_current_thread`]：给音频线程打下手的线程（URB 收发、主时钟跟随）用普通的最高优先级。
//!   这类线程随槽位数增加，最多三十多个，不占 MMCSS 每进程的线程名额。

use std::io;
use std::marker::PhantomData;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Threading::{
    AVRT_PRIORITY_HIGH, AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, AvSetMmThreadPriority,
    GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
};
use windows::core::w;

/// 持有期间线程留在 MMCSS 任务中，析构时退出。必须在加入任务的线程上析构，所以不能跨线程移动。
#[derive(Debug)]
pub struct RealtimeGuard {
    task: HANDLE,
    _same_thread: PhantomData<*const ()>,
}

/// 让当前线程加入 MMCSS 的 "Pro Audio" 任务。
pub fn promote_current_thread() -> io::Result<RealtimeGuard> {
    let mut task_index = 0u32;
    // SAFETY: 任务名是有效的以 NUL 结尾的宽字符串；`task_index` 在调用期间有效。
    let task = unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task_index) }?;
    let guard = RealtimeGuard {
        task,
        _same_thread: PhantomData,
    };
    // SAFETY: `task` 是刚取得的有效任务句柄。
    unsafe { AvSetMmThreadPriority(guard.task, AVRT_PRIORITY_HIGH) }?;
    Ok(guard)
}

impl Drop for RealtimeGuard {
    fn drop(&mut self) {
        // SAFETY: 句柄由本对象独占，且类型保证在加入任务的线程上析构。
        let _ = unsafe { AvRevertMmThreadCharacteristics(self.task) };
    }
}

/// 把当前线程提到普通优先级范围内的最高档，线程结束前一直有效。
pub fn raise_current_thread() -> io::Result<()> {
    // SAFETY: `GetCurrentThread` 返回的伪句柄总是有效，不需要关闭。
    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL) }?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_can_join_and_leave_the_pro_audio_task() {
        std::thread::spawn(|| {
            drop(promote_current_thread().unwrap());
            raise_current_thread().unwrap();
        })
        .join()
        .unwrap();
    }
}
