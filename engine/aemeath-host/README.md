# aemeath-host

引擎进程（`aemeath-host.exe`）：组装引擎与 USB/IP 服务端，在命名管道上与主程序通信，写日志，缓存最近一份路由拓扑并在启动时先恢复出声。

```
aemeath-host [--data-dir <目录>] [--usbip-port <端口>] [--pipe <管道名>] [--meters <共享内存名>]
```

正式运行时由守护进程拉起并传入参数；开发时可以直接运行，按 Ctrl+C 退出。参数解析和日志由三个进程共用的 `aemeath-runtime` 提供，主程序接受同样的参数。

## 默认值

| 项 | 默认 |
|---|---|
| 数据目录 | `%APPDATA%\Aemeath Dev`（正式版为 `%APPDATA%\Aemeath`，安装版和便携版相同） |
| USB/IP 端口 | 32410（经守护进程启动时由它决定） |
| 管道名 | `\\.\pipe\Aemeath.Dev.Engine.<当前用户 SID>` |
| 电平共享内存 | `Local\Aemeath.Dev.Meters.<当前用户 SID>` |

默认的管道名、共享内存名和数据目录都由 `aemeath_usbip::identity::IdentityProfile` 计算，主程序用同一套函数，两边结果一致。

## 线程与优先级

进程启动时改用高优先级类（`HIGH_PRIORITY_CLASS`），并关闭电源节流（EcoQoS），不需要管理员。

| 线程 | 数量 | 作用 | 优先级 |
|---|---|---|---|
| `aemeath-scheduler` | 1 | 唯一的实时线程：完成所有槽位的等时 URB，每 1 ms 运行一次混音，替换拓扑 | MMCSS Pro Audio，Critical |
| `aemeath-input`、`aemeath-output` | 每台外部设备一个 | WASAPI 设备流与 ASRC，经环形缓冲与调度线程交换音频 | MMCSS Pro Audio，Critical |
| `aemeath-app-capture` | 每个应用源的每个根进程一个 | 进程回环采集与 ASRC | MMCSS Pro Audio，Critical |
| `usbip-conn-<N>` | 每条 USB/IP 连接一个 | 读取并解析 URB，交给调度线程 | `TIME_CRITICAL`（15） |
| `usbip-write-<N>` | 每条 USB/IP 连接一个 | 按顺序写回 URB 的应答 | `TIME_CRITICAL`（15） |
| `aemeath-clock-follow` | 1 | 按主时钟设备调整时间线的速率 | `TIME_CRITICAL`（15） |
| 主线程 | 1 | 命名管道 IPC、拓扑校验与缓存、构建混音器、每 20 ms 转写电平 | 普通（13） |
| `aemeath-host-events` | 1 | 把引擎事件转给主线程 | 普通（13） |
| `aemeath-resampler-builder` | 1 | 在实时线程之外构建和释放槽位的重采样器 | 普通（13） |
| `aemeath-recorder` | 每个录音节点一个 | 从环形缓冲取出录音数据并写盘 | 普通（13） |
| `aemeath-endpoint-watch` | 1 | 监听端点增减和状态变化，唤醒离线设备的重试 | 普通（13） |
| `aemeath-app-find` | 每个应用源一个 | 按程序路径查找进程，为根进程开采集线程 | 普通（13） |
| `usbip-listen` | 1 | 接受 USB/IP 连接 | 普通（13） |
| 日志写入线程 | 1 | 把日志写进文件（非阻塞） | 普通（13） |

- MMCSS 线程由系统的 MMCSS 服务放进实时优先级范围（16 以上），与进程的优先级类无关。
- 括号里的数字是高优先级类下的线程优先级：普通线程是 13，`TIME_CRITICAL` 是 15。
- USB/IP 的两类线程随连接数增加（16 个槽位最多三十多个），所以不进 MMCSS，以免占满每个进程的 MMCSS 线程名额。

## 命名管道

- 只允许当前用户访问（受保护的 DACL，只有当前用户一条授权），拒绝远程客户端。
- 第一个实例要求"首个实例"：同名管道已存在（被别的进程或另一个引擎占用）时启动失败，这也保证每个用户只运行一个引擎。
- 之后始终保留一个等待连接的实例，避免两次会话之间被别的进程抢先创建同名管道。
- 同一时间服务一个客户端（主程序）；协议见 `crates/aemeath-protocol/README.md`。连接后 5 秒内必须握手；握手后先收到一份状态快照（槽位连接与格式、外部设备状态），之后收到变化。

## 电平

- 实时线程只写进程内的电平组。另有一个转写任务，每 20 ms 把峰值累加进共享内存，布局见 `aemeath_protocol::meters`。
- 共享内存只允许当前用户访问。
- 引擎重启时，如果主程序还开着上一块共享内存，就直接沿用，主程序这边不中断。
- 共享内存建不起来时只记录日志，音频照常运行。

## 数据目录

| 路径 | 内容 |
|---|---|
| `logs\engine.<日期>.log` | 按天轮转，保留 15 天；非阻塞写入，实时线程不会被磁盘 IO 拖慢 |
| `logs\panic.engine.log` | panic 时同步追加（发布构建 `panic = "abort"`，进程随即中止并由守护进程拉起）；最后一次写入超过 15 天后清理 |
| `engine\topology.json` | 最近一份成功应用的路由拓扑。先写临时文件再改名；启动时读取并重新校验 |
