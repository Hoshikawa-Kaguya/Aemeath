//! 把程序图标嵌入引擎（任务管理器里显示）。

fn main() {
    println!("cargo::rerun-if-changed=host.rc");
    println!("cargo::rerun-if-changed=../../app/src-tauri/icons/icon.ico");
    if let Err(error) = embed_resource::compile("host.rc", embed_resource::NONE).manifest_optional() {
        println!("cargo::error=cannot embed the engine icon: {error}");
    }
}
