//! 主窗口的最小尺寸与上次的位置和大小。
//!
//! 最小尺寸是主显示器分辨率的 65%（维护者 2026-11-08 要求）：工作台、路由矩阵、控制台三栏同屏，窗口太小时布局会挤坏；
//! 按主显示器的物理分辨率算，不同分辨率和缩放比例下占屏幕的比例一致。
//! tauri.conf.json 里的 minWidth / minHeight 只是拿不到显示器信息时的下限。
//!
//! 窗口在 tauri.conf.json 里是隐藏的，摆好位置后才显示，否则会先在屏幕中央闪一下。

use std::path::PathBuf;
use std::time::Duration;

use aemeath_core::preferences::WindowPlacement;
use aemeath_core::store::ConfigStore;
use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize, Runtime, WebviewWindow};

use crate::controller::{ControllerHandle, Edit};
use crate::tray::MAIN_WINDOW;

/// 数据目录（Tauri 托管状态）。配置、日志、WebView2 的用户数据都在这里（M4 计划 D1、D6）。
pub struct DataDir(pub PathBuf);

impl DataDir {
    pub fn webview(&self) -> PathBuf {
        self.0.join(WEBVIEW_FOLDER)
    }
}

const WEBVIEW_FOLDER: &str = "webview";
/// 界面没有报告就绪时，最多等这么久就显示窗口
const SHOW_FALLBACK: Duration = Duration::from_secs(3);
/// 最小尺寸占主显示器分辨率的比例
const MIN_SIZE_RATIO: f64 = 0.65;
/// 判断窗口是否在屏幕内时，取标题栏中央的这一点（标题栏高 48 逻辑像素，这里取一半）
const TITLE_BAR_PROBE_PX: i32 = 24;

/// 显示器的矩形（物理像素）
#[derive(Debug, Clone, Copy)]
struct Screen {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

/// 新建主窗口后调用：设置最小尺寸，恢复上次的位置和大小，然后显示。
pub fn prepare<R: Runtime>(window: &WebviewWindow<R>) {
    apply_min_size(window);
    let saved = window
        .try_state::<ConfigStore>()
        .and_then(|store| store.peek_preferences())
        .and_then(|preferences| preferences.window);
    if let Some(placement) = saved {
        restore(window, placement);
    }
    // 等界面应用好主题后再显示（window_ready），否则会闪默认色（M4 计划 V4）；页面加载失败时兜底显示，窗口不能一直看不见
    let fallback = window.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(SHOW_FALLBACK).await;
        show(&fallback);
    });
}

/// 显示主窗口。
pub fn show<R: Runtime>(window: &WebviewWindow<R>) {
    if let Err(error) = window.show() {
        tracing::warn!(%error, "cannot show the main window");
    }
}

fn restore<R: Runtime>(window: &WebviewWindow<R>, placement: WindowPlacement) {
    let screens: Vec<Screen> = window
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|monitor| Screen {
            x: monitor.position().x,
            y: monitor.position().y,
            width: monitor.size().width,
            height: monitor.size().height,
        })
        .collect();
    if !title_bar_on_screen(placement, &screens) {
        tracing::info!("the saved window position is off every screen, using the default");
        return;
    }
    // 先移动再设大小：移到缩放比例不同的显示器时，Windows 会按新比例调整窗口大小
    let _ = window.set_position(PhysicalPosition::new(placement.x, placement.y));
    let _ = window.set_size(PhysicalSize::new(placement.width, placement.height));
    if placement.maximized {
        let _ = window.maximize();
    }
}

/// 标题栏中央在某台显示器内时才恢复（显示器可能已被拔掉），否则窗口会打开在看不见的地方。
fn title_bar_on_screen(placement: WindowPlacement, screens: &[Screen]) -> bool {
    let x = i64::from(placement.x) + i64::from(placement.width) / 2;
    let y = i64::from(placement.y) + i64::from(TITLE_BAR_PROBE_PX);
    screens.iter().any(|screen| {
        let (left, top) = (i64::from(screen.x), i64::from(screen.y));
        (left..left + i64::from(screen.width)).contains(&x) && (top..top + i64::from(screen.height)).contains(&y)
    })
}

/// 读取主窗口当前的位置和大小。窗口不存在或最小化时返回 None（最小化时读到的坐标没有意义）。
pub fn capture(app: &AppHandle) -> Option<WindowPlacement> {
    let window = app.get_webview_window(MAIN_WINDOW)?;
    if window.is_minimized().unwrap_or(true) {
        return None;
    }
    let position = window.outer_position().ok()?;
    let size = window.inner_size().ok()?;
    Some(WindowPlacement {
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
        maximized: window.is_maximized().unwrap_or(false),
    })
}

/// 把 [`capture`] 的结果写进配置。
pub async fn remember(app: AppHandle, placement: Option<WindowPlacement>) {
    let (Some(placement), Some(controller)) = (placement, app.try_state::<ControllerHandle>()) else {
        return;
    };
    if let Err(error) = controller.edit(Edit::SetWindowPlacement(placement)).await {
        tracing::warn!(?error, "cannot save the window placement");
    }
}

/// 设置最小尺寸；窗口当前比它小时（例如在高分辨率显示器上用默认尺寸打开），放大到最小尺寸并居中。
fn apply_min_size<R: Runtime>(window: &WebviewWindow<R>) {
    let monitor = match window.primary_monitor() {
        Ok(Some(monitor)) => monitor,
        Ok(None) => {
            tracing::warn!("no primary monitor, keeping the configured minimum window size");
            return;
        }
        Err(error) => {
            tracing::warn!(%error, "cannot read the primary monitor, keeping the configured minimum window size");
            return;
        }
    };
    let screen = monitor.size();
    let scaled = |pixels: u32| (f64::from(pixels) * MIN_SIZE_RATIO).round() as u32;
    let minimum = PhysicalSize::new(scaled(screen.width), scaled(screen.height));
    if let Err(error) = window.set_min_size(Some(minimum)) {
        tracing::warn!(%error, "cannot set the minimum window size");
        return;
    }
    let Ok(current) = window.inner_size() else {
        return;
    };
    if current.width < minimum.width || current.height < minimum.height {
        let enlarged = PhysicalSize::new(current.width.max(minimum.width), current.height.max(minimum.height));
        let _ = window.set_size(enlarged);
        let _ = window.center();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRIMARY: Screen = Screen {
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
    };
    /// 主显示器左侧的副显示器（坐标为负）
    const LEFT: Screen = Screen {
        x: -1920,
        y: 0,
        width: 1920,
        height: 1080,
    };

    fn at(x: i32, y: i32) -> WindowPlacement {
        WindowPlacement {
            x,
            y,
            width: 1200,
            height: 800,
            maximized: false,
        }
    }

    #[test]
    fn restores_only_when_the_title_bar_is_visible() {
        assert!(title_bar_on_screen(at(100, 100), &[PRIMARY]));
        assert!(title_bar_on_screen(at(-1500, 100), &[PRIMARY, LEFT]));
        assert!(
            !title_bar_on_screen(at(-1500, 100), &[PRIMARY]),
            "that monitor was unplugged"
        );
        assert!(
            !title_bar_on_screen(at(100, -200), &[PRIMARY]),
            "title bar above the screen"
        );
        assert!(!title_bar_on_screen(at(100, 100), &[]));
    }
}
