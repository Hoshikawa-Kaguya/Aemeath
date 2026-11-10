fn main() {
    // tauri-build 不监听图标文件和 tauri.conf.json 引用的版本文件：只改它们时增量构建会沿用旧值（2026-11-11 实测图标）
    println!("cargo::rerun-if-changed=icons");
    println!("cargo::rerun-if-changed=../../scripts/version.json");
    tauri_build::build();
}
