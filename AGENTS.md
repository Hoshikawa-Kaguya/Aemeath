# Aemeath 维护手册

给加功能、修问题的 agent 和维护者看。项目介绍、目录结构、构建和检查命令见 [README.md](README.md) 的"开发"一节，这里不重复。

## 先读

- [GLOSSARY.md](GLOSSARY.md)：领域术语。代码、界面文案、文档里的叫法都以它为准，`_Avoid_` 里列的词不要用。
- 本地有 `agent_tmp/` 时（不公开，已列入 `.gitignore`）：先读 `agent_tmp/README.md`。关键决策在 `agent_tmp/docs/adr/`，技术现状在 `agent_tmp/docs/选型报告.md`，待办在 `agent_tmp/docs/待办.md`。

## 改动流程

- **修 bug、小改动**：直接改，不写计划；改完在 `agent_tmp/docs/修改记录.md` 的"未发布"下记一条（日期、类型、改了什么、为什么、怎么验证的）。
- **需要设计讨论的功能**：先在 `agent_tmp/docs/` 写一份可以脱离对话独立执行的计划（`<功能名>计划.md`：设计决定、工作项、进度），做完后移入 `agent_tmp/archive/docs/`，并在修改记录里记一条，附上计划的路径。
- **待办**：从 `待办.md` 里拿来做的事，做完就从待办里删掉；决定不做的也直接删掉。
- **术语**：用词有变化，或者出现新概念时，当场更新 `GLOSSARY.md`。术语表只写概念的含义，不写实现细节。
- **ADR**：只有同时满足"难以回退、没有上下文时让人意外、确实在几个方案之间做了取舍"时才写，接着 `agent_tmp/docs/adr/` 的编号。

## 完成标准

每次改动都要通过 README 里列的全部检查：

- 根目录：`cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、`cargo deny check`
- 动到前端时，在 `app/` 下：`pnpm format:check && pnpm lint && pnpm build`

新的非平凡逻辑（分支、解析、状态判定）要有单元测试。只能在真机或虚拟机上验证的，写明没有验证，记进修改记录；需要维护者验证的放进 `待办.md`。

## 测试的边界

- agent 跑的测试每项不超过 5 分钟。
- 需要 UAC 的操作、会改动系统默认设备的操作、涉及游戏和反作弊的测试，先问维护者。
- 开发和测试只用开发身份（`Aemeath Dev In/Out N`，不带参数运行时的默认身份），不要临时改序列号，否则会在系统里留下新的设备实例。需要清理时用 `tools/aemeath-dev-cleanup`。
- 不要假设某台设备存在：代码和测试都不能写死设备名、采样率、声道数。测试时避开正被其他程序独占的设备。
- PowerShell 脚本用 PowerShell 7（`pwsh`）运行。Windows PowerShell 5.1 会按 ANSI 读取无 BOM 的 UTF-8 脚本，中文会把代码吞掉。

## 代码约定

- **Rust**
  - 只有 `crates/aemeath-sys` 可以写 `unsafe`（其余 crate 由 workspace lint 禁止），新的 Windows API 调用都封装在这里。
  - 不用 `unwrap`、`expect`、`panic`（clippy 会拒绝，测试代码除外）；错误要往上传或记日志。
  - 引擎的调度线程 `aemeath-scheduler` 是唯一的实时线程：里面不加锁等待、不分配内存、不做阻塞 IO。耗时的准备工作交给非实时线程（例如重采样器的构建、录音写盘），用无锁结构交接。日志是非阻塞的，可以写，但要限频。
  - 按职责拆 crate，每个 crate 能单独测试；不要为了少写几行把职责合并到一个 crate。
- **前端**
  - 新增的 Tauri 命令要同时：在 `app/src-tauri/src/main.rs` 注册；在 `app/src/api/backend.ts` 加调用；在 `app/src/dev/preview.ts` 加模拟处理，否则 `pnpm preview` 会报错。
  - 界面文案以英文为源语言，`en.json` 和 `zh-CN.json` 同时加同样的键，并在 `app/src/i18n/context.json` 给译者写一句上下文说明。文案只用命名占位符，不拼接字符串。
  - 颜色、字号、圆角只用 `app/src/styles/tokens.css` 里的变量（来自 Fluent 2 设计令牌）。
- **文件**：UTF-8 无 BOM、LF 行尾、以换行结尾（`.cmd`、`.bat` 用 CRLF），由 `.editorconfig` 和 `.gitattributes` 固定。

## 注释

- 用自然语言写，简洁、说清楚就好。
- 能自注释的代码不写注释；不要逐行复述代码在做什么。
- 复杂的逻辑写清楚这段代码做了什么，以及为什么这样做（例如实测得到的约束、绕开的系统行为、参数的取值理由）。
- `pub` 的函数和类型写一句文档注释说明用途；名字已经说清楚的（简单的 getter、构造函数）不写。
- 不写对话或讨论的过程，不写聊天里的代号：ADR 编号、里程碑（M1–M5）、spike 编号（S1–S7）、切片号、计划里的决定编号等。也不引用 `agent_tmp/` 下的路径。这些都不公开，读代码的人看不到。

## 兼容性

已经有用户在用，升级不能丢掉用户的配置。

- **配置**（`app/crates/aemeath-core/src/store.rs`）
  - 加字段时用 serde 默认值做到向后兼容，不升版本号。
  - 结构不兼容时才把 `CONFIG_VERSION` 加一，并且必须在 `ConfigStore::load` 里写从上一个版本迁移的代码，配上测试。不允许"解析失败就备份、从空配置开始"。
  - 只保留从上一个版本迁移的代码：升到新版本时，删掉更早的迁移。跨越多个版本的旧配置会被备份，然后从空配置开始。
- **协议**（`crates/aemeath-protocol`）：IPC 消息有任何不兼容的变化都把 `PROTOCOL_VERSION` 加一。引擎和主程序总是一起发布，握手要求版本完全一致，不需要迁移；引擎缓存的旧拓扑校验不通过时会被丢弃。

## 版本号与发布

- 改版本号时两处一起改，保持相同：
  - `scripts/version.json` 的 `version`：程序里显示的版本、exe 的文件版本和安装包版本都来自它（代码读 `aemeath_runtime::VERSION`，`tauri.conf.json` 的 `version` 指向这个文件，`scripts/release.ps1` 也读它）。
  - 根目录 `Cargo.toml` 中 `[workspace.package]` 的 `version`：各 crate 的版本。
- 格式是语义化版本 `主.次.修订`，可带 `-预发布` 后缀，不带 `v` 前缀；格式不对时构建会报错。
- 推送 `v主.次.修订[-后缀]` tag 后，`.github/workflows/release.yml` 核对 tag 与两处版本号，打包并发布到 GitHub Releases；带后缀的标为预发布。tag 由维护者推送。
- 发版顺序：改版本号 → 维护者手动运行 `/changelog`（本地 skill：比较上一个 tag 与 HEAD，写入 `CHANGELOG.md`，并把修改记录的"未发布"改成版本号）→ 提交 → 打 tag 并推送。CI 把 `CHANGELOG.md` 里这个版本的一节写进 GitHub Release 的说明。
- `CHANGELOG.md` 只增量更新：新版本插在文件顶部的标记下面。不要读整个文件，用 `grep -n '^## '` 找到需要的那一节再按行号读。版本标题只用 `## `，版本内部的标题从 `###` 开始。

## 文档

- 公开文档有 `README.md`、`docs/usage.md` 和它们的英文版（`README.en.md`、`docs/usage.en.md`）。功能有变化时中英文一起改，界面上的叫法与语言包保持一致。
- 文档写"现在怎么做"。被替代的做法直接改写成现在的做法，不留"原来是什么、后来取消了"。只有在没有替代做法时，才写"不做 X"或"不支持 X"。
