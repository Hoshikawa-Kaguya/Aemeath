//! 程序的图标（Shell 显示的那个大图标），转成 RGBA 像素供界面显示。

use std::ffi::c_void;
use std::path::Path;

use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject,
    GetDIBits, GetObjectW, HBITMAP, HDC,
};
use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
use windows::Win32::UI::Shell::{SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGetFileInfoW};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};
use windows::core::HSTRING;

use crate::com::ComApartment;

/// 一张 RGBA 图像，按行从上到下。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// 文件的大图标（通常 32×32）；文件不存在、没有彩色图标或读取失败时为 None。
pub fn program_icon(path: &Path) -> Option<RgbaImage> {
    // SHGetFileInfo 要求调用线程已初始化 COM
    let _apartment = ComApartment::multithreaded().ok()?;
    let mut info = SHFILEINFOW::default();
    // SAFETY: info 是有效的可写结构，大小如实传入；成功时 hIcon 归我们所有，下面销毁
    let found = unsafe {
        SHGetFileInfoW(
            &HSTRING::from(path.as_os_str()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut info),
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        )
    };
    if found == 0 || info.hIcon.is_invalid() {
        return None;
    }
    let image = icon_pixels(info.hIcon);
    // SAFETY: hIcon 来自上面的 SHGetFileInfoW，此后不再使用
    let _ = unsafe { DestroyIcon(info.hIcon) };
    image
}

fn icon_pixels(icon: HICON) -> Option<RgbaImage> {
    let mut parts = ICONINFO::default();
    // SAFETY: icon 是有效的图标句柄；成功时两张位图归我们所有，下面删除
    unsafe { GetIconInfo(icon, &mut parts) }.ok()?;
    let image = if parts.hbmColor.is_invalid() {
        None
    } else {
        color_and_mask(parts.hbmColor, parts.hbmMask)
    };
    for bitmap in [parts.hbmColor, parts.hbmMask] {
        if !bitmap.is_invalid() {
            // SAFETY: 位图来自 GetIconInfo，此后不再使用
            let _ = unsafe { DeleteObject(bitmap.into()) };
        }
    }
    image
}

fn color_and_mask(color: HBITMAP, mask: HBITMAP) -> Option<RgbaImage> {
    let mut header = BITMAP::default();
    // SAFETY: header 是有效的可写 BITMAP，大小如实传入
    let written = unsafe {
        GetObjectW(
            color.into(),
            size_of::<BITMAP>() as i32,
            Some((&raw mut header).cast::<c_void>()),
        )
    };
    if written == 0 || header.bmWidth <= 0 || header.bmHeight <= 0 {
        return None;
    }
    let (width, height) = (header.bmWidth as u32, header.bmHeight as u32);
    // SAFETY: 传 None 创建与屏幕兼容的内存 DC，下面删除
    let dc = unsafe { CreateCompatibleDC(None) };
    if dc.is_invalid() {
        return None;
    }
    let mut pixels = read_bgra(dc, color, width, height);
    // 旧式图标没有 alpha 通道，透明区域由掩码给出（掩码为白处透明）
    if let Some(pixels) = pixels.as_mut()
        && pixels.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 0)
    {
        let mask = read_bgra(dc, mask, width, height);
        for (index, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let transparent = mask.as_ref().is_some_and(|mask| mask[index * 4] != 0);
            pixel[3] = if transparent { 0 } else { u8::MAX };
        }
    }
    // SAFETY: dc 是上面创建的内存 DC，此后不再使用
    let _ = unsafe { DeleteDC(dc) };
    let mut pixels = pixels?;
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Some(RgbaImage { width, height, pixels })
}

/// 把位图读成自上而下的 32 位 BGRA。
fn read_bgra(dc: HDC, bitmap: HBITMAP, width: u32, height: u32) -> Option<Vec<u8>> {
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            // 负高度表示自上而下
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    // SAFETY: pixels 的大小正好是 width × height 个 32 位像素；info 描述了这块缓冲的格式
    let lines = unsafe {
        GetDIBits(
            dc,
            bitmap,
            0,
            height,
            Some(pixels.as_mut_ptr().cast::<c_void>()),
            &mut info,
            DIB_RGB_COLORS,
        )
    };
    (lines == height as i32).then_some(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_icon_of_an_executable() {
        let path = std::env::current_exe().unwrap();
        let icon = program_icon(&path).expect("every executable has at least the default icon");
        assert!(icon.width > 0 && icon.height > 0);
        assert_eq!(icon.pixels.len(), (icon.width * icon.height * 4) as usize);
        assert!(
            icon.pixels.as_chunks::<4>().0.iter().any(|pixel| pixel[3] > 0),
            "not fully transparent"
        );
    }

    #[test]
    fn a_missing_file_has_no_icon() {
        assert_eq!(program_icon(Path::new(r"C:\does\not\exist\nothing.exe")), None);
    }
}
