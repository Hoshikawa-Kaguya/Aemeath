# 第三方声明

Aemeath 以 [AGPL-3.0-only](LICENSE) 发布。发布包附带或链接了以下第三方组件。

## 随发布包分发

- **usbip-win2**（BSD-2-Clause）：虚拟 USB 主控制器驱动的官方安装程序，放在发布包的 `redist\` 目录，由 Aemeath 在用户同意后安装。源码：<https://github.com/vadimgrn/usbip-win2>

## 链接进程序

- Rust 依赖：每次打包时由 `scripts/release.ps1` 生成完整清单（名称、版本、许可证），放在发布包的 `licenses\rust-dependencies.txt`。
- 前端依赖：见 `app/package.json`。

其中 `ring`（经 Velopack 的 HTTPS 下载引入）包含 C 和汇编代码，其余 Rust 依赖不含 C 代码。
