//! 主程序的单实例与启动转交。只在从安装包运行（安装版或便携版）时生效，开发构建保持原样。
//!
//! - 守护进程拉起的主程序：已有主程序在运行（守护进程意外退出后留下的）时，请它显示窗口，然后以退出码 0 退出。
//!   守护进程把 0 当作用户退出、随之结束，不会反复拉起新的主程序。
//! - 被直接启动的主程序（任务栏固定了 aemeath-app.exe，或快捷方式指向了它）：已有实例时同样请它显示窗口；
//!   没有实例时改为启动同目录的守护进程，由它按正常流程拉起引擎和主程序。
//! - 带 `--autostart` 的重复启动只退出，不弹窗口。
//!
//! 唤起用按身份和用户 SID 命名的事件（`aemeath_sys::activation`），没有用 tauri-plugin-single-instance：
//! 它按 tauri.conf.json 的 identifier 命名，开发身份和正式身份同名，两种身份同时运行时会互相吞掉对方的启动。

use aemeath_runtime::config::{PIPE_ARGUMENT, RuntimeConfig};
use aemeath_sys::activation::{self, ActivationEvent};
use tauri::AppHandle;
use tracing::{error, info, warn};
use velopack::locator::{LocationContext, auto_locate_app_manifest};

use crate::commands::GUARDIAN_EXE;
use crate::tray;

/// 启动检查的结论。
pub enum Startup {
    /// 继续启动。持有唤起事件时，主程序要在后台等待它并显示窗口；开发构建或取不到事件时为 None
    Run(Option<ActivationEvent>),
    /// 已交给已运行的主程序或守护进程处理，直接退出
    Exit,
}

/// 在创建任何窗口之前调用：决定继续启动还是退出，并设置任务栏身份。`args` 是不含程序名的启动参数。
pub fn claim(config: &RuntimeConfig, args: &[String]) -> Startup {
    let Ok(locator) = auto_locate_app_manifest(LocationContext::FromCurrentExe) else {
        info!("not running from a package, single-instance handling is off");
        return Startup::Run(None);
    };
    // 与 Velopack 给开始菜单快捷方式设置的相同，任务栏才把窗口和快捷方式归为一组
    if let Err(error) = activation::set_app_user_model_id(&locator.get_app_user_model_id()) {
        warn!(%error, "cannot set the AppUserModelID");
    }
    let name = match config.identity.app_activation_name() {
        Ok(name) => name,
        Err(error) => {
            warn!(%error, "cannot name the activation event, running without single-instance handling");
            return Startup::Run(None);
        }
    };
    let event = match ActivationEvent::create(&name) {
        Ok(event) => event,
        Err(error) => {
            warn!(%error, "cannot create the activation event, running without single-instance handling");
            return Startup::Run(None);
        }
    };
    match event {
        Some(event) if launched_by_guardian(args) => Startup::Run(Some(event)),
        Some(event) => {
            // 先放开事件：守护进程拉起的主程序要能创建它
            drop(event);
            relay_to_guardian()
        }
        None => {
            if !config.autostart {
                match activation::request_activation(&name) {
                    Ok(_) => info!("another app is running, asked it to show its window"),
                    Err(error) => error!(%error, "cannot ask the running app to show its window"),
                }
            }
            Startup::Exit
        }
    }
}

/// 在后台线程上调用：每次被唤起就到主线程上显示主窗口（窗口已销毁时重新创建）。
/// 等待出错时停止，此后重复启动不再弹出窗口，主程序照常运行。
pub fn show_on_activation(event: &ActivationEvent, app: &AppHandle) {
    loop {
        if let Err(error) = event.wait() {
            error!(%error, "cannot wait for activation, repeated launches will no longer show the window");
            return;
        }
        let target = app.clone();
        if let Err(error) = app.run_on_main_thread(move || tray::show_main_window(&target)) {
            warn!(%error, "cannot show the main window");
        }
    }
}

/// 守护进程拉起主程序时总会传入管道名；直接启动（任务栏、快捷方式）时没有参数。
fn launched_by_guardian(args: &[String]) -> bool {
    args.iter().any(|arg| arg == PIPE_ARGUMENT)
}

/// 改为启动同目录的守护进程。启动不了时退回到直接运行，至少让用户看到窗口。
fn relay_to_guardian() -> Startup {
    let guardian = std::env::current_exe().map(|path| path.with_file_name(GUARDIAN_EXE));
    match guardian.and_then(|program| std::process::Command::new(program).spawn()) {
        Ok(_) => {
            info!("started directly, handed over to the guardian");
            Startup::Exit
        }
        Err(error) => {
            error!(%error, "cannot start the guardian, running without it");
            Startup::Run(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_owned()).collect()
    }

    /// 只有带管道名参数才算守护进程拉起的；没有参数或只有 --autostart 都算直接启动
    #[test]
    fn only_the_pipe_argument_marks_a_guardian_launch() {
        assert!(launched_by_guardian(&args(&[
            "--data-dir",
            "D:",
            "--pipe",
            r"\\.\pipe\x",
            "--autostart"
        ])));
        assert!(!launched_by_guardian(&args(&[])));
        assert!(!launched_by_guardian(&args(&["--autostart"])));
    }
}
