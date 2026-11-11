# aemeath-fuzz

对 USB/IP 解析做模糊测试（`cargo-fuzz` + libFuzzer）。USB/IP 服务端监听 127.0.0.1，本机任何进程都能连上，所以解析代码收到的字节完全不可信。

这是单独的 workspace：需要 nightly，不进主 workspace 的构建、CI 和发布。

| 目标 | 内容 |
|---|---|
| `op_header` | 连接建立后的第一个请求头 |
| `busid` | 导入请求里的 busid；解析成功的还要能原样再解析一次 |
| `urb_stream` | URB 阶段的字节流；一次性喂入和按任意大小分块喂入，解出的命令必须相同 |
| `control` | 在一个模拟槽位上依次执行一串控制请求 |

## 运行

```sh
rustup toolchain install nightly --profile minimal
cargo install cargo-fuzz
# 在仓库根目录
cargo +nightly fuzz run urb_stream -- -max_total_time=240
```

Windows 上默认开启 AddressSanitizer，运行时需要 MSVC 的 `clang_rt.asan_dynamic-x86_64.dll`。它随 Visual Studio 的 C++ 工具安装，但不在 PATH 里，运行前把它所在的目录加进 PATH，例如：

```sh
export PATH="/c/Program Files/Microsoft Visual Studio/18/Community/VC/Tools/MSVC/<版本>/bin/Hostx64/x64:$PATH"
```

语料（`corpus/`）和发现的崩溃输入（`artifacts/`）只留在本机，不提交。发现崩溃时，修复后把那份输入改写成 `aemeath-usbip` 的单元测试。
