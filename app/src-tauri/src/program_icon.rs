//! `icon` 协议：界面用 `http://icon.localhost/<编码后的程序路径>` 取程序图标（PNG）。
//!
//! 图标不放进状态快照：快照每秒推一次，图标每个 1–4 KB。走协议时浏览器自己缓存，取不到就显示通用图标。

use std::path::Path;

use aemeath_protocol::ids::ProgramPath;
use aemeath_sys::icon::{RgbaImage, program_icon};
use percent_encoding::percent_decode_str;
use tauri::http::{Response, StatusCode, header};

pub const SCHEME: &str = "icon";

/// 处理一次请求：`uri_path` 是 URL 的路径部分（以 `/` 开头，经过百分号编码）。会读文件，在阻塞线程里调用。
pub fn respond(uri_path: &str) -> Response<Vec<u8>> {
    let png = program_path(uri_path)
        .and_then(|path| program_icon(Path::new(path.as_str())))
        .and_then(|image| encode_png(&image));
    let response = match png {
        Some(png) => Response::builder()
            .header(header::CONTENT_TYPE, "image/png")
            .header(header::CACHE_CONTROL, "max-age=86400")
            .body(png),
        None => Response::builder().status(StatusCode::NOT_FOUND).body(Vec::new()),
    };
    response.unwrap_or_else(|_| Response::new(Vec::new()))
}

/// 只接受程序路径（绝对路径、`.exe`），其他一律拒绝。
fn program_path(uri_path: &str) -> Option<ProgramPath> {
    let decoded = percent_decode_str(uri_path.strip_prefix('/')?).decode_utf8().ok()?;
    ProgramPath::new(&decoded).ok()
}

fn encode_png(image: &RgbaImage) -> Option<Vec<u8>> {
    let mut png = Vec::new();
    let mut encoder = png::Encoder::new(&mut png, image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(&image.pixels).ok()?;
    writer.finish().ok()?;
    Some(png)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_absolute_exe_paths_are_served() {
        assert_eq!(
            program_path("/C%3A%5CProgram%20Files%5CApp%5Capp.exe").map(|path| path.as_str().to_owned()),
            Some(r"C:\Program Files\App\app.exe".to_owned())
        );
        assert!(program_path("/app.exe").is_none(), "relative");
        assert!(program_path("/C%3A%5Cnotes.txt").is_none(), "not a program");
        assert!(program_path("/C%3A%5C%FF.exe").is_none(), "not UTF-8");
        assert!(program_path("C%3A%5Capp.exe").is_none(), "no leading slash");
    }

    #[test]
    fn encodes_rgba_as_png() {
        let image = RgbaImage {
            width: 2,
            height: 1,
            pixels: vec![255, 0, 0, 255, 0, 0, 255, 128],
        };
        let png = encode_png(&image).unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(png)).read_info().unwrap();
        let mut decoded = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut decoded).unwrap();
        assert_eq!((info.width, info.height), (2, 1));
        assert_eq!(decoded, image.pixels);
    }

    #[test]
    fn unknown_programs_are_not_found() {
        assert_eq!(
            respond("/C%3A%5Cdoes%5Cnot%5Cexist.exe").status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(respond("/notes.txt").status(), StatusCode::NOT_FOUND);
    }
}
