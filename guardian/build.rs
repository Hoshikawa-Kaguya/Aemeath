//! 把清单嵌入守护进程（TaskDialog 需要 Common Controls 6）。

fn main() {
    println!("cargo::rerun-if-changed=guardian.rc");
    println!("cargo::rerun-if-changed=guardian.manifest");
    println!("cargo::rerun-if-changed=../app/src-tauri/icons/icon.ico");
    if let Err(error) = embed_resource::compile("guardian.rc", embed_resource::NONE).manifest_required() {
        println!("cargo::error=cannot embed the guardian manifest: {error}");
    }
}
