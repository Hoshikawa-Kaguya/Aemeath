//! 拉起并监视引擎与主程序（ADR-0003、ADR-0008）。
//!
//! - 先拉起引擎，再拉起主程序。
//! - 任一进程异常退出时按退避间隔重新拉起；连续运行超过 [`Timing::healthy_after`] 后退避清零。
//! - 主程序以退出码 0 退出表示用户从托盘退出：它已经请引擎停止，这里等引擎退出（超时就结束它），然后整体停止。
//! - 引擎以退出码 0 退出通常也是用户正在退出：先等 [`Timing::quit_grace`]，主程序在这期间退出就不再拉起引擎。
//!
//! 没有把子进程放进作业对象：守护进程自己异常退出时，引擎和主程序继续运行，音频不中断。

use std::io;
use std::process::{Child, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

use tracing::{error, info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Engine,
    App,
}

#[derive(Debug, Clone)]
pub struct Timing {
    pub poll: Duration,
    /// 第 n 次连续失败后的重启间隔（超过长度时用最后一项）
    pub backoff: Vec<Duration>,
    pub healthy_after: Duration,
    pub quit_grace: Duration,
    /// 用户退出时等引擎自行退出的时间
    pub stop_timeout: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            poll: Duration::from_millis(200),
            backoff: [1, 2, 4, 8, 16, 30].map(Duration::from_secs).to_vec(),
            healthy_after: Duration::from_secs(60),
            quit_grace: Duration::from_secs(3),
            stop_timeout: Duration::from_secs(5),
        }
    }
}

/// 监视结束时的统计（日志和测试用）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Report {
    pub engine_launches: usize,
    pub app_launches: usize,
}

struct Managed {
    role: Role,
    child: Option<Child>,
    started: Instant,
    failures: usize,
    next_start: Instant,
    launches: usize,
}

impl Managed {
    fn new(role: Role, now: Instant) -> Self {
        Self {
            role,
            child: None,
            started: now,
            failures: 0,
            next_start: now,
            launches: 0,
        }
    }

    fn start_if_due(&mut self, launch: &mut impl FnMut(Role) -> io::Result<Child>, timing: &Timing) {
        let now = Instant::now();
        if self.child.is_some() || now < self.next_start {
            return;
        }
        self.launches += 1;
        match launch(self.role) {
            Ok(child) => {
                info!(role = ?self.role, pid = child.id(), "started");
                self.child = Some(child);
                self.started = now;
            }
            Err(error) => {
                error!(role = ?self.role, %error, "cannot start");
                self.schedule_restart(now, timing, Duration::ZERO);
            }
        }
    }

    /// 进程已退出时返回退出状态。
    fn exited(&mut self) -> Option<ExitStatus> {
        let child = self.child.as_mut()?;
        match child.try_wait() {
            Ok(Some(status)) => {
                self.child = None;
                Some(status)
            }
            Ok(None) => None,
            Err(error) => {
                warn!(role = ?self.role, %error, "cannot query the process");
                None
            }
        }
    }

    fn schedule_restart(&mut self, now: Instant, timing: &Timing, minimum: Duration) {
        if now.duration_since(self.started) >= timing.healthy_after {
            self.failures = 0;
        }
        self.failures += 1;
        let index = (self.failures - 1).min(timing.backoff.len().saturating_sub(1));
        let delay = timing.backoff.get(index).copied().unwrap_or_default().max(minimum);
        info!(role = ?self.role, failures = self.failures, restart_in = ?delay, "will restart");
        self.next_start = now + delay;
    }
}

/// 监视直到用户退出（主程序以退出码 0 退出）。`launch` 负责启动对应的进程。
pub fn supervise(mut launch: impl FnMut(Role) -> io::Result<Child>, timing: &Timing) -> Report {
    let now = Instant::now();
    let mut engine = Managed::new(Role::Engine, now);
    let mut app = Managed::new(Role::App, now);
    loop {
        engine.start_if_due(&mut launch, timing);
        app.start_if_due(&mut launch, timing);

        if let Some(status) = engine.exited() {
            let now = Instant::now();
            warn!(?status, "the engine exited");
            // 正常退出多半是用户正在退出：留出时间让主程序先退出
            let minimum = if status.success() {
                timing.quit_grace
            } else {
                Duration::ZERO
            };
            engine.schedule_restart(now, timing, minimum);
        }
        if let Some(status) = app.exited() {
            if status.success() {
                info!("the app exited normally (quit from the tray), stopping");
                stop(&mut engine, timing);
                return Report {
                    engine_launches: engine.launches,
                    app_launches: app.launches,
                };
            }
            warn!(?status, "the app exited unexpectedly");
            app.schedule_restart(Instant::now(), timing, Duration::ZERO);
        }
        thread::sleep(timing.poll);
    }
}

/// 等引擎自行退出（主程序退出前已请它停止），超时就结束它。
fn stop(engine: &mut Managed, timing: &Timing) {
    let Some(child) = engine.child.as_mut() else {
        return;
    };
    let deadline = Instant::now() + timing.stop_timeout;
    while Instant::now() < deadline {
        if let Ok(Some(status)) = child.try_wait() {
            info!(?status, "the engine stopped");
            return;
        }
        thread::sleep(timing.poll);
    }
    warn!("the engine did not stop in time, terminating it");
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    fn fast() -> Timing {
        Timing {
            poll: Duration::from_millis(5),
            backoff: vec![Duration::from_millis(20), Duration::from_millis(40)],
            healthy_after: Duration::from_secs(5),
            quit_grace: Duration::from_millis(50),
            stop_timeout: Duration::from_millis(200),
        }
    }

    fn exits_with(code: i32) -> io::Result<Child> {
        Command::new("cmd")
            .args(["/c", "exit", &code.to_string()])
            .stdout(Stdio::null())
            .spawn()
    }

    /// 运行约 `seconds` 秒后以 0 退出
    fn runs_for(seconds: u32) -> io::Result<Child> {
        Command::new("ping")
            .args(["-n", &(seconds + 1).to_string(), "127.0.0.1"])
            .stdout(Stdio::null())
            .spawn()
    }

    #[test]
    fn a_normal_app_exit_stops_everything() {
        let started = Instant::now();
        let report = supervise(
            |role| match role {
                Role::Engine => runs_for(30),
                Role::App => exits_with(0),
            },
            &fast(),
        );
        assert_eq!(
            report,
            Report {
                engine_launches: 1,
                app_launches: 1
            }
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the engine was terminated, not waited for"
        );
    }

    #[test]
    fn a_crashing_app_is_restarted_with_backoff() {
        let mut app_runs = 0;
        let started = Instant::now();
        let report = supervise(
            |role| match role {
                Role::Engine => runs_for(30),
                Role::App => {
                    app_runs += 1;
                    exits_with(if app_runs < 3 { 3 } else { 0 })
                }
            },
            &fast(),
        );
        assert_eq!(report.app_launches, 3);
        assert_eq!(report.engine_launches, 1);
        assert!(
            started.elapsed() >= Duration::from_millis(60),
            "20 ms + 40 ms of backoff"
        );
    }

    #[test]
    fn a_crashing_engine_is_restarted_while_the_app_runs() {
        let report = supervise(
            |role| match role {
                Role::Engine => exits_with(3),
                Role::App => runs_for(1),
            },
            &fast(),
        );
        assert!(report.engine_launches >= 3, "{report:?}");
        assert_eq!(report.app_launches, 1);
    }
}
