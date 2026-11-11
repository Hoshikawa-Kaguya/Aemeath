# aemeath-usbip

USB/IP 服务端与 UAC2 设备模型：把 16 个固定槽位（`Aemeath In 1–8`、`Aemeath Out 1–8`）作为 USB 音频设备提供给 usbip-win2。

不含 DSP，也不含时间线和调度。等时 URB 以 `ServerEvent::IsoSubmit` 交给引擎，由主时钟调度线程按时间线完成。

## 模块

| 模块 | 内容 |
|---|---|
| `wire` | USB/IP 报文解析与编码。纯字节处理，所有长度和包数都有上限；这是对外的攻击面，用 `cargo-fuzz` 做模糊测试（仓库根目录的 `fuzz/`） |
| `identity` | 槽位的固定 USB 身份。**任何一项都不能改**，否则会产生新的设备实例 |
| `descriptors` | UAC2 描述符。输入槽位是扬声器，输出槽位是线路；声明全部采样率和 5 种样本格式。改动描述符时必须同时修改 `identity` 中的设备版本号 |
| `function` | 控制请求处理（描述符、采样率、备用设置），以及流的开始、停止和格式变化 |
| `stream` | 流格式，以及录音方向每个等时包的帧数（44.1k 系列按余数累计，长期不漂移） |
| `server` | 只监听 127.0.0.1；只接受已导出槽位的导入，重复导入返回"设备忙"（状态 2），未导出返回"无此设备"（状态 4）；连接数和握手阶段都有限制 |

## 线程与所有权

- 每条连接一个读线程，解析 URB，在本地处理控制请求。
- 导入成功后，连接的写端随 `ServerEvent::Imported` 交给调度线程；此后只有调度线程写这条连接，控制请求的应答也经事件转交。
- `UsbipServer::set_exported` 设定被导出的槽位（即路由拓扑中被占用的槽位）；不再导出的槽位会被立即断开。

## 冒烟测试

`examples/enumerate.rs` 导出指定的开发槽位、只应答控制请求，用来在本机实际挂载，检查描述符和设备身份：

```sh
cargo run -p aemeath-usbip --example enumerate -- 32411 30 in1 out1
# 另开终端
"C:\Program Files\USBip\usbip.exe" -t 32411 attach -r 127.0.0.1 -b 1-1 --once
```

挂载后，设备管理器里应出现已有的 `Aemeath Dev In 1` 实例，名称不带"2- "后缀。
