//! 日志：写入数据目录下的 `logs\`，每个进程一组文件（`engine.<日期>.log`、`app.<日期>.log`……），
//! 按天轮转，保留 15 天；同时输出到标准错误（开发时在控制台查看）。
//!
//! 写文件用非阻塞写入器：引擎的调度线程等记日志时不会被磁盘 IO 拖慢（ADR-0008）。
//! 日志是维护者长期使用时排查问题的依据，所以统一带 UTC 时间戳，不使用颜色控制字符。

use std::io::{self, Write};
use std::path::Path;
use std::time::{Duration, SystemTime};

use tracing::level_filters::LevelFilter;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// 日志保留天数。按天轮转，一直运行时文件数上限就等于天数；隔天使用时由 [`remove_expired_logs`] 按时间清理
const RETAINED_LOG_DAYS: u64 = 15;
const SECONDS_PER_DAY: u64 = 86_400;
const LOG_RETENTION: Duration = Duration::from_secs(RETAINED_LOG_DAYS * SECONDS_PER_DAY);
const LOG_FILE_SUFFIX: &str = "log";
/// panic 时同步追加写入的文件名前缀（非阻塞写入器在进程中止前可能来不及写出）：`panic.engine.log` 等。
/// 不能以进程名开头，否则轮转器会把它算进文件个数，当成最旧的文件删掉
const PANIC_LOG_PREFIX: &str = "panic";
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
        .max_log_files(RETAINED_LOG_DAYS as usize)
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
    remove_expired_logs(log_dir, SystemTime::now());
    install_panic_hook(&log_dir.join(format!(
        "{PANIC_LOG_PREFIX}.{}.{LOG_FILE_SUFFIX}",
        process.file_prefix()
    )));
    Ok(LogGuard {
        _file: worker,
        _console: console_worker,
    })
}

/// 删除最后写入时间超过保留期的日志（三个进程的都算，过期文件不会有人在写）。
/// 轮转器只按个数清理，断断续续使用时几个月前的文件还算在 15 个之内。
fn remove_expired_logs(log_dir: &Path, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(log_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let expired = path.extension().is_some_and(|extension| extension == LOG_FILE_SUFFIX)
            && entry.metadata().is_ok_and(|metadata| {
                metadata.is_file()
                    && metadata
                        .modified()
                        .is_ok_and(|modified| now.duration_since(modified).is_ok_and(|age| age > LOG_RETENTION))
            });
        if expired && let Err(error) = std::fs::remove_file(&path) {
            tracing::warn!(path = %path.display(), %error, "cannot remove an expired log file");
        }
    }
}

/// panic 时先同步写一份到 `panic.<进程>.log`，再交给 tracing（发布构建 `panic = "abort"`，随后进程中止，由守护进程拉起）。
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};

    #[test]
    fn only_expired_log_files_are_removed() {
        let dir = std::env::temp_dir().join(format!("aemeath-log-retention-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let now = SystemTime::now();
        let expired_at = now - LOG_RETENTION - Duration::from_secs(SECONDS_PER_DAY);
        let within_retention = now - LOG_RETENTION + Duration::from_secs(SECONDS_PER_DAY);
        let write = |name: &str, modified: SystemTime| {
            File::create(dir.join(name)).unwrap().set_modified(modified).unwrap();
        };
        write("engine.2026-10-01.log", expired_at);
        write("panic.engine.log", expired_at);
        write("app.2026-10-27.log", within_retention);
        write("guardian.2026-11-10.log", now);
        write("notes.txt", expired_at);

        remove_expired_logs(&dir, now);

        let mut left: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(left, ["app.2026-10-27.log", "guardian.2026-11-10.log", "notes.txt"]);
    }
}
