//! 音频相关进程和线程的优先级。
//!
//! - [`prioritize_current_process`]：引擎进程用高优先级类，并关闭电源节流。
//! - [`promote_current_thread`]：真正处理音频的线程（调度线程、设备 IO 线程）加入 MMCSS 的 "Pro Audio" 任务（High 类别），
//!   并取任务内的最高优先级。默认的 "Audio" 任务只是 Medium 类别。
//! - [`raise_current_thread`]：给音频线程打下手的线程（URB 收发、主时钟跟随）用普通的最高优先级。
//!   这类线程随槽位数增加，最多三十多个，不占 MMCSS 每进程的线程名额。

use std::io;
use std::marker::PhantomData;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Threading::{
    AVRT_PRIORITY_CRITICAL, AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, AvSetMmThreadPriority,
    GetCurrentProcess, GetCurrentThread, HIGH_PRIORITY_CLASS, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
    PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
    PROCESS_POWER_THROTTLING_STATE, ProcessPowerThrottling, SetPriorityClass, SetProcessInformation, SetThreadPriority,
    THREAD_PRIORITY_TIME_CRITICAL,
};
use windows::core::w;

/// 当前进程改用高优先级类（不需要管理员），并关闭电源节流。
///
/// 没有窗口的进程会被 Windows 11 当作后台进程节流（降频、排到能效核）。两步各自尝试，返回第一个错误。
pub fn prioritize_current_process() -> io::Result<()> {
    // SAFETY: `GetCurrentProcess` 返回的伪句柄总是有效，不需要关闭。
    let class = unsafe { SetPriorityClass(GetCurrentProcess(), HIGH_PRIORITY_CLASS) };
    // 控制位置 1、状态位为 0：明确声明"不节流"，而不是交给系统决定
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
        StateMask: 0,
    };
    // SAFETY: 结构体在调用期间有效，大小与类型一致。
    let throttling = unsafe {
        SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            std::ptr::from_ref(&state).cast(),
            u32::try_from(size_of::<PROCESS_POWER_THROTTLING_STATE>()).unwrap_or(u32::MAX),
        )
    };
    class?;
    throttling?;
    Ok(())
}

/// 持有期间线程留在 MMCSS 任务中，析构时退出。必须在加入任务的线程上析构，所以不能跨线程移动。
#[derive(Debug)]
pub struct RealtimeGuard {
    task: HANDLE,
    _same_thread: PhantomData<*const ()>,
}

/// 让当前线程加入 MMCSS 的 "Pro Audio" 任务，取任务内的最高优先级（Critical）。
pub fn promote_current_thread() -> io::Result<RealtimeGuard> {
    let mut task_index = 0u32;
    // SAFETY: 任务名是有效的以 NUL 结尾的宽字符串；`task_index` 在调用期间有效。
    let task = unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task_index) }?;
    let guard = RealtimeGuard {
        task,
        _same_thread: PhantomData,
    };
    // SAFETY: `task` 是刚取得的有效任务句柄。
    unsafe { AvSetMmThreadPriority(guard.task, AVRT_PRIORITY_CRITICAL) }?;
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

    #[test]
    fn the_process_can_be_prioritized_without_elevation() {
        prioritize_current_process().unwrap();
    }
}
