//! 程序版本号来自打包脚本旁边的 `scripts/version.json`：三个进程、exe 的文件版本（Tauri 直接读同一个文件）
//! 和安装包都用这一个值。用 JSON 是因为 `tauri.conf.json > version` 只接受 JSON 文件的路径。

use std::path::PathBuf;

fn main() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/version.json");
    println!("cargo::rerun-if-changed={}", path.display());
    let version = std::fs::read_to_string(&path)
        .map_err(|error| error.to_string())
        .and_then(|text| {
            // 记事本等编辑器可能加上 BOM
            serde_json::from_str::<serde_json::Value>(text.trim_start_matches('\u{feff}'))
                .map_err(|error| error.to_string())
        })
        .and_then(|json| {
            json["version"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "it needs a \"version\" string".to_owned())
        });
    match version {
        Ok(version) if is_semver(&version) => println!("cargo::rustc-env=AEMEATH_VERSION={version}"),
        Ok(version) => println!(
            "cargo::error={} must hold a version like 1.2.3, found {version:?}",
            path.display()
        ),
        Err(error) => println!("cargo::error=cannot read {}: {error}", path.display()),
    }
}

/// `主.次.修订`，可带 `-预发布` 后缀：Velopack 和 Tauri 都要求语义化版本。
fn is_semver(version: &str) -> bool {
    let core = match version.split_once('-') {
        Some((core, pre)) if !pre.is_empty() => core,
        Some(_) => return false,
        None => version,
    };
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}
