//! 主窗口的最小尺寸：主显示器分辨率的 65%（维护者 2026-11-08 要求）。
//!
//! 工作台、路由矩阵、控制台三栏同屏，窗口太小时布局会挤坏；按主显示器的物理分辨率算，
//! 不同分辨率和缩放比例下占屏幕的比例一致。tauri.conf.json 里的 minWidth / minHeight 只是拿不到显示器信息时的下限。

use tauri::{PhysicalSize, Runtime, WebviewWindow};

/// 最小尺寸占主显示器分辨率的比例
const MIN_SIZE_RATIO: f64 = 0.65;

/// 设置最小尺寸；窗口当前比它小时（例如在高分辨率显示器上用默认尺寸打开），放大到最小尺寸并居中。
pub fn apply_min_size<R: Runtime>(window: &WebviewWindow<R>) {
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
