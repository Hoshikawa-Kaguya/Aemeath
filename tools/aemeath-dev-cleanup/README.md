# aemeath-dev-cleanup

开发工具：完全卸载开发身份（`Aemeath Dev In/Out 1–8`）的设备实例，把 S1 的结论产品化（ADR-0009）。

```
cargo run -p aemeath-dev-cleanup              # 预演：列出将要卸载的实例，不需要管理员权限
cargo run -p aemeath-dev-cleanup -- --apply   # 在管理员终端里执行卸载
```

- **卸载顺序**：音频端点（`SWD\MMDEVAPI\…`）→ 音频功能子设备（`USB\VID_1209&PID_000x&MI_00\…`）→ USB 复合设备（`USB\VID_1209&PID_000x\AEMDEV…`），每个实例用 `pnputil /remove-device` 卸载。
- **只碰开发槽位**：
  - 复合设备按 VID/PID 和序列号精确匹配；
  - 功能子设备按父实例归属；
  - 端点按设备接口名精确匹配。

  使用同一个测试 PID 的其他设备（例如探针）不会被卸载。
- **不动 MMDevices 记录**：S1 实测，"2- "这类后缀只取决于是否存在同名的设备实例，留下的 MMDevices 记录是无害的历史条目。
- **执行前要先退出 Aemeath**：驱动里还挂着指向 Aemeath 的设备时拒绝执行。
- **没有管理员权限时拒绝执行**，不会自己弹出 UAC。

卸载后再启动 Aemeath，会重新注册 16 个槽位（首次注册），设备名不带后缀。

查找和卸载的逻辑在 `aemeath_control::slots::uninstall`。M4 里安装版的卸载钩子和便携版的"移除虚拟设备"调用同一段逻辑，只是换成正式身份。
