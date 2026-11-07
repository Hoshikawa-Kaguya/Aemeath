//! 主程序（ADR-0003、ADR-0013）：Rust 后端负责控制面，前端运行在 WebView2 里，窗口完全由前端绘制。

// 发布版不弹出控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;

fn main() -> ExitCode {
    match tauri::Builder::default().run(tauri::generate_context!()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // 日志在 M1 的 aemeath-app 任务中接入；在此之前只能输出到调试控制台
            eprintln!("Aemeath failed to start: {error}");
            ExitCode::FAILURE
        }
    }
}
