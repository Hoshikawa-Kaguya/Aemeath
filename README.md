# Aemeath

Aemeath 是一个 Windows 11 上的虚拟混音台：用节点和连线，把麦克风、应用和其他外部设备的声音路由、调整后，送往耳机和音箱，或者送往供 OBS、Discord 等应用录制的虚拟设备。

> 状态：开发中，尚未发布。

## 特点

- **节点图工作台**：自由连接输入、总线和输出，另有路由矩阵和控制台两种快速调整视图。
- **自带虚拟设备**：不需要另装虚拟线缆。Aemeath 提供 8 个虚拟输入（`Aemeath In 1–8`）和 8 个虚拟输出（`Aemeath Out 1–8`），通过已有微软签名的 [usbip-win2](https://github.com/vadimgrn/usbip-win2) 和 Windows 自带的 USB 声卡驱动实现，不需要测试签名，也不需要关闭安全启动。
- **按应用分轨**：把不同应用的声音分别送进不同的虚拟输入，或者在应用照常播放的同时复制一份它的声音。
- **高品质**：内部 32 位浮点处理；设备格式最高 384 kHz（独占模式 768 kHz）；所有采样率转换都在 Aemeath 内部完成，可以选择重采样算法。
- **低延迟**：外部设备默认以 WASAPI 独占模式打开，格式和缓冲可以按设备设定；也可以对单个设备改用共享模式。
- **Windows Fluent 风格界面**，支持中文和英文。

术语见 [`GLOSSARY.md`](GLOSSARY.md)。

## 系统要求

- Windows 11 x64
- usbip-win2 驱动：已随 Aemeath 附带，首次启动时会引导安装（需要管理员权限一次）

## 发布形式

- **安装版**：运行 `Setup.exe` 即可，无需管理员权限。
- **便携版**：解压即用，配置保存在程序文件夹内。删除便携版前，请先在设置页执行"移除虚拟设备"。

## 开发

需要：Rust（stable，1.90 以上）、Node.js 22.12 以上、pnpm（版本见 `app/package.json` 的 `packageManager`）。

```sh
cd app
pnpm install
pnpm tauri dev                # 开发运行主程序（前端热更新）
pnpm tauri build --no-bundle  # 发布构建；安装包由 Velopack 另行打包
```

主程序要连上引擎才能出声。守护进程会依次拉起引擎和主程序；开发时也可以手动先运行引擎（`cargo run -p aemeath-host`），再运行主程序。两者不带参数时，按开发身份算出相同的数据目录（`%APPDATA%\Aemeath Dev`）、管道名和共享内存名。

不启动后端、只检查界面时，在 `app/` 下运行 `pnpm preview`，它会启动开发服务器并在浏览器中打开 `http://localhost:21364/preview.html`：预览页用 Tauri 的 `mockIPC` 模拟后端（`src/dev/preview.ts`），只由开发服务器提供，不进入发布构建。

检查（与 CI 相同）：

```sh
# 在 app/ 下
pnpm format:check && pnpm lint && pnpm build
# 在仓库根目录下（需要先执行一次 pnpm build：主程序编译时要嵌入 app/dist）
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

目录结构：

| 目录 | 内容 |
|---|---|
| `crates/` | 共用的库：`aemeath-protocol`（IPC 消息、路由拓扑、电平共享内存布局）、`aemeath-sys`（Windows API 封装，唯一允许 `unsafe` 的 crate）、`aemeath-runtime`（三个进程共用的命令行参数与日志） |
| `engine/` | 引擎进程：音频、重采样、主时钟、USB/IP 服务端 |
| `app/` | 主程序（Tauri 2）：`src/` 是 Vue 3 前端，`src-tauri/` 是 Rust 后端，`crates/` 是控制面的库 |
| `guardian/` | 守护进程 |
| `tools/` | 开发工具：`aemeath-dev-cleanup`（完全卸载开发槽位的设备实例） |

## 许可证

[AGPL-3.0-only](LICENSE)
