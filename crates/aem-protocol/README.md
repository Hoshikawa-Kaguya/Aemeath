# aem-protocol

引擎（`aem-host`）与主程序（`aem-app`）之间的 IPC 消息定义。两端直接依赖这个 crate，消息格式只有这一份定义。

## 线上格式

- 传输：命名管道，只允许当前用户访问。
- 分帧：4 字节小端长度 + UTF-8 JSON，单帧上限 1 MiB（`frame::MAX_FRAME_LEN`）。
- 握手：主程序先发 `hello`，协议版本（`PROTOCOL_VERSION`）必须与引擎完全相同，否则引擎回 `hello_rejected` 并断开。
- 命令带 `request` 编号，引擎回 `ack` 或 `rejected`；设备状态等事件由引擎主动推送。

示例：

```json
{"type":"hello","protocol":1,"app_version":"0.1.0"}
{"type":"apply_plan","request":7,"plan":{"revision":3,"engine_sample_rate":48000,"clock_master":5,"resampler":"sinc_standard","nodes":[...],"routes":[...]}}
{"type":"endpoint_status","node":3,"status":{"state":"offline","reason":"in_use_by_other_app"}}
{"type":"slot_connection","slot":{"direction":"out","number":8},"connected":true}
```

## 校验

- 带语义的数值（槽位编号、采样率、增益）在反序列化时就校验范围。
- 所有消息都拒绝未知字段和未知类型。
- 路由计划用 `RoutePlan::validate` 检查结构：主程序编译计划后、引擎收到计划时都要调用。

## 修改规则

消息格式有任何不兼容的变化，都要把 `PROTOCOL_VERSION` 加一，并更新 `message.rs` 中的线上格式测试。
