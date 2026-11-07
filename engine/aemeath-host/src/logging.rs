//! 日志：写入数据目录下的 `logs\`，按天轮转，保留 14 天；同时输出到标准错误（开发时在控制台查看）。
//!
//! 写文件用非阻塞写入器：调度线程等实时线程记日志时不会被磁盘 IO 拖慢（ADR-0008）。
//! 日志是维护者长期使用时排查问题的依据，所以统一带 UTC 时间戳，不使用颜色控制字符。

use std::io::{self, Write};
use std::path::Path;

use tracing::level_filters::LevelFilter;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// 保留的日志文件数（按天轮转，即 14 天）
const RETAINED_LOG_FILES: usize = 14;
const LOG_FILE_PREFIX: &str = "engine";
const LOG_FILE_SUFFIX: &str = "log";
/// panic 时同步追加写入的文件（非阻塞写入器在进程中止前可能来不及写出）
const PANIC_LOG_FILE: &str = "panic.log";

/// 持有到进程结束：析构时把尚未写出的日志刷到文件。
pub struct LogGuard {
    _worker: WorkerGuard,
}

/// 初始化日志。重复初始化（例如测试中）时只建立文件写入器，不替换已有的全局订阅者。
pub fn init(log_dir: &Path) -> io::Result<LogGuard> {
    std::fs::create_dir_all(log_dir)?;
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(LOG_FILE_PREFIX)
        .filename_suffix(LOG_FILE_SUFFIX)
        .max_log_files(RETAINED_LOG_FILES)
        .build(log_dir)
        .map_err(io::Error::other)?;
    let (writer, worker) = tracing_appender::non_blocking(appender);
    let file_layer = tracing_subscriber::fmt::layer().with_writer(writer).with_ansi(false);
    let console_layer = tracing_subscriber::fmt::layer().with_writer(io::stderr);
    let _ = tracing_subscriber::registry()
        .with(LevelFilter::INFO)
        .with(file_layer)
        .with(console_layer)
        .try_init();
    install_panic_hook(log_dir);
    Ok(LogGuard { _worker: worker })
}

/// panic 时先同步写一份到 `panic.log`，再交给 tracing（发布构建 `panic = "abort"`，随后进程中止，由守护进程拉起）。
fn install_panic_hook(log_dir: &Path) {
    let path = log_dir.join(PANIC_LOG_FILE);
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(file, "{:?}: {info}\n{backtrace}", std::time::SystemTime::now());
        }
        tracing::error!(%info, "panic");
    }));
}
