# Aemeath 代理须知

## 版本号

- 改版本号时两处一起改，保持相同：
  - `scripts/version.json` 的 `version`：程序里显示的版本、exe 的文件版本和安装包版本都来自它（代码读 `aemeath_runtime::VERSION`，`tauri.conf.json` 的 `version` 指向这个文件，`scripts/release.ps1` 也读它）。
  - 根目录 `Cargo.toml` 中 `[workspace.package]` 的 `version`：各 crate 的版本。
- 格式是语义化版本 `主.次.修订`，可带 `-预发布` 后缀，不带 `v` 前缀；格式不对时构建会报错。
