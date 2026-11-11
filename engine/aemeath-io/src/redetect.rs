//! 离线设备的重新检测。
//!
//! - 定时重试：节点进入离线后，前 [`FAST_RETRY_FOR`] 每 [`FAST_RETRY`] 一次，之后每 [`SLOW_RETRY`] 一次。
//!   其他程序释放设备时系统不发通知，只能靠定时重试发现。
//! - 立即重试：端点增减或状态变化的通知（`IMMNotificationClient`）、用户的"重新检测"都会唤醒所有等待中的 IO 线程。
//! - "设备不允许独占"不做定时重试（重试无用），只在被唤醒时重试。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use aemeath_protocol::ids::{EndpointId, ProgramPath};

/// 刚离线时的重试间隔：设备多半只是被拔出又插回，尽快恢复出声
pub const FAST_RETRY: Duration = Duration::from_secs(2);
/// 离线超过这么久就改用慢速重试
pub const FAST_RETRY_FOR: Duration = Duration::from_secs(120);
/// 长时间离线后的重试间隔：设备可能已经不在了，不必频繁打扰系统
pub const SLOW_RETRY: Duration = Duration::from_secs(10);
/// 等待期间检查停止信号的间隔
const STOP_POLL: Duration = Duration::from_millis(100);

/// 被检测的对象：外部设备的端点，或者应用源的程序。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedetectTarget {
    Endpoint(EndpointId),
    Program(ProgramPath),
}

/// 唤醒的范围
#[derive(Debug, Clone, PartialEq, Eq)]
enum Scope {
    All,
    Targets(Vec<RedetectTarget>),
}

#[derive(Debug, Default)]
struct State {
    /// 每次唤醒加一；等待方记下开始等待时的值，变了就说明被唤醒过
    generation: u64,
    scope: Option<Scope>,
}

/// 所有 IO 线程共享的唤醒信号。
#[derive(Debug, Clone, Default)]
pub struct Redetect {
    shared: Arc<(Mutex<State>, Condvar)>,
}

impl Redetect {
    /// 唤醒全部离线设备（端点变化通知、托盘的"重新检测全部设备"）。
    pub fn wake_all(&self) {
        self.wake(Scope::All);
    }

    /// 只唤醒这些对象（节点上的"重新检测"）。
    pub fn wake_targets(&self, targets: Vec<RedetectTarget>) {
        self.wake(Scope::Targets(targets));
    }

    /// 递增代数并唤醒所有等待者，各自按 `scope` 判断这次是不是叫自己。
    fn wake(&self, scope: Scope) {
        let (lock, condvar) = &*self.shared;
        let mut state = lock.lock().unwrap_or_else(PoisonError::into_inner);
        state.generation += 1;
        state.scope = Some(scope);
        condvar.notify_all();
    }

    /// 等待 `timeout`（None 表示只等唤醒），被唤醒、超时或收到停止信号时返回。
    pub fn wait(&self, target: &RedetectTarget, timeout: Option<Duration>, stop: &AtomicBool) {
        let deadline = timeout.map(|timeout| Instant::now() + timeout);
        let (lock, condvar) = &*self.shared;
        let mut state = lock.lock().unwrap_or_else(PoisonError::into_inner);
        let started = state.generation;
        loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            if state.generation != started && applies(state.scope.as_ref(), target) {
                return;
            }
            let step = match deadline {
                Some(deadline) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return;
                    }
                    remaining.min(STOP_POLL)
                }
                None => STOP_POLL,
            };
            state = condvar
                .wait_timeout(state, step)
                .map(|(state, _)| state)
                .unwrap_or_else(|poisoned| poisoned.into_inner().0);
        }
    }
}

/// 这次重新检测是否包含 `target`。
fn applies(scope: Option<&Scope>, target: &RedetectTarget) -> bool {
    match scope {
        None | Some(Scope::All) => true,
        Some(Scope::Targets(targets)) => targets.contains(target),
    }
}

/// 离线了多久决定下一次定时重试的间隔。
pub fn retry_interval(offline_for: Duration) -> Duration {
    if offline_for < FAST_RETRY_FOR {
        FAST_RETRY
    } else {
        SLOW_RETRY
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn endpoint(id: &str) -> RedetectTarget {
        RedetectTarget::Endpoint(EndpointId(id.to_owned()))
    }

    #[test]
    fn retries_quickly_at_first_then_slowly() {
        assert_eq!(retry_interval(Duration::ZERO), FAST_RETRY);
        assert_eq!(retry_interval(FAST_RETRY_FOR), SLOW_RETRY);
    }

    #[test]
    fn a_wake_ends_the_wait_early() {
        let redetect = Redetect::default();
        let stop = AtomicBool::new(false);
        let waker = redetect.clone();
        let started = Instant::now();
        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            waker.wake_all();
        });
        redetect.wait(&endpoint("mic"), Some(Duration::from_secs(10)), &stop);
        assert!(started.elapsed() < Duration::from_secs(5));
        handle.join().unwrap();
    }

    #[test]
    fn a_wake_for_another_endpoint_does_not_end_the_wait() {
        let redetect = Redetect::default();
        let stop = AtomicBool::new(false);
        let waker = redetect.clone();
        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            waker.wake_targets(vec![endpoint("speaker")]);
        });
        let started = Instant::now();
        redetect.wait(&endpoint("mic"), Some(Duration::from_millis(300)), &stop);
        assert!(started.elapsed() >= Duration::from_millis(300));
        handle.join().unwrap();
    }
}
