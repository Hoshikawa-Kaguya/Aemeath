//! 日志：写入数据目录下的 `logs\`，每个进程一组文件（`engine.<日期>.log`、`app.<日期>.log`……），
//! 按天轮转，保留 14 天；同时输出到标准错误（开发时在控制台查看）。
//!
//! 写文件用非阻塞写入器：引擎的调度线程等记日志时不会被磁盘 IO 拖慢（ADR-0008）。
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
const LOG_FILE_SUFFIX: &str = "log";
/// panic 时同步追加写入的文件名后缀（非阻塞写入器在进程中止前可能来不及写出）：`engine.panic.log` 等
const PANIC_LOG_SUFFIX: &str = "panic.log";
/// 数据目录下的日志文件夹
pub const LOG_FOLDER: &str = "logs";

/// 写日志的进程。三个进程共用一个日志文件夹，按文件名区分。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Process {
    Engine,
    App,
    Guardian,
}

impl Process {
    fn file_prefix(self) -> &'static str {
        match self {
            Self::Engine => "engine",
            Self::App => "app",
            Self::Guardian => "guardian",
        }
    }
}

/// 持有到进程结束：析构时把尚未写出的日志刷到文件。
pub struct LogGuard {
    _file: WorkerGuard,
    _console: WorkerGuard,
}

/// 初始化日志，写到 `data_dir` 下的 [`LOG_FOLDER`]。重复初始化（例如测试中）时只建立文件写入器，不替换已有的全局订阅者。
pub fn init(data_dir: &Path, process: Process) -> io::Result<LogGuard> {
    let log_dir = data_dir.join(LOG_FOLDER);
    let log_dir = log_dir.as_path();
    std::fs::create_dir_all(log_dir)?;
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(process.file_prefix())
        .filename_suffix(LOG_FILE_SUFFIX)
        .max_log_files(RETAINED_LOG_FILES)
        .build(log_dir)
        .map_err(io::Error::other)?;
    let (writer, worker) = tracing_appender::non_blocking(appender);
    let file_layer = tracing_subscriber::fmt::layer().with_writer(writer).with_ansi(false);
    // 实时线程也写日志，而终端或管道可能卡住几百毫秒（2026-11-08 实测调度线程因此停了 187 ms、285 ms），
    // 所以控制台和文件一样交给后台线程，写不过来时丢弃
    let (console_writer, console_worker) = tracing_appender::non_blocking(io::stderr());
    let console_layer = tracing_subscriber::fmt::layer().with_writer(console_writer);
    let _ = tracing_subscriber::registry()
        .with(LevelFilter::INFO)
        .with(file_layer)
        .with(console_layer)
        .try_init();
    install_panic_hook(&log_dir.join(format!("{}.{PANIC_LOG_SUFFIX}", process.file_prefix())));
    Ok(LogGuard {
        _file: worker,
        _console: console_worker,
    })
}

/// panic 时先同步写一份到 `<进程>.panic.log`，再交给 tracing（发布构建 `panic = "abort"`，随后进程中止，由守护进程拉起）。
fn install_panic_hook(path: &Path) {
    let path = path.to_path_buf();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(file, "{:?}: {info}\n{backtrace}", std::time::SystemTime::now());
        }
        tracing::error!(%info, "panic");
    }));
}
