//! 引擎进程入口。用法：`aem-host [--data-dir <目录>] [--usbip-port <端口>] [--pipe <管道名>]`
//!
//! 正式运行时由守护进程拉起并监视（ADR-0003）；异常退出后守护进程会重新拉起，引擎按缓存的计划先恢复出声。

use std::process::ExitCode;

use aem_host::config::HostConfig;
use aem_host::logging;

const LOG_FOLDER: &str = "logs";

fn main() -> ExitCode {
    let config = match HostConfig::from_args(std::env::args().skip(1)) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("aem-host: {error}");
            return ExitCode::FAILURE;
        }
    };
    let _logs = match logging::init(&config.data_dir.join(LOG_FOLDER)) {
        Ok(guard) => guard,
        Err(error) => {
            eprintln!("aem-host: cannot open the log directory: {error}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!(%error, "cannot start the async runtime");
            return ExitCode::FAILURE;
        }
    };
    let shutdown = async {
        // 开发时在控制台按 Ctrl+C 退出；注册失败时只靠 shutdown 命令退出
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
        tracing::info!("Ctrl+C received");
    };
    match runtime.block_on(aem_host::run(config, shutdown)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "engine host failed");
            ExitCode::FAILURE
        }
    }
}
