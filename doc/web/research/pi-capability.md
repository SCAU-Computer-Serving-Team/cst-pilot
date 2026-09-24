# Pi 能力清单

状态：已按 0.85.1 核对，并用官方 Windows 二进制验证最小接管路径。更新：2026-09-23。

本文记录本项目实际依赖的 Pi 能力，作为实现依据。依据项目安装的 `@earendil-works/pi-coding-agent` 0.85.1，不以全局 Pi 版本作为依据。设计见[会话运行与并行](../SPEC/session-runtime.md)与[页面地址与接口设计](../SPEC/app-router.md)，决策见[技术决策](tech-decisions.md)。

## 可行性核对

| 核对点 | 结论 | 本地源码位置 |
|---|---|---|
| SDK 创建会话 | 每次 `createAgentSession()` 创建新的 `Agent` 与 `AgentSession`，可为不同会话分别提供 `SessionManager` | `dist/core/sdk.js` |
| 单会话输入 | Pi 0.85.1 提供 `steer` / `followUp` 底层投递机制，不在同一会话内并发启动两轮独立执行；本项目的用户可见输入队列自行管理，不将底层投递机制视为完整的产品队列 | `dist/core/agent-session.js` 的 `prompt()` |
| 取消范围 | `AgentSession.abort()` 取消自身 Agent 与相关重试、压缩等工作；阻断排队消息与后续执行由本项目队列层负责 | `dist/core/agent-session.js` |
| 内置会话替换 | `switchSession()` / `newSession()` 经 `teardownCurrent()` 中断并销毁旧实例，不能用于 Web 内普通页面导航 | `dist/core/agent-session-runtime.js` |
| 消息与工具事件 | 提供 `message_*` 与 `tool_execution_*`，Web 可按会话订阅并渲染 | `dist/core/agent-session.js` |
| 原生 TUI | `/web` 可在 TUI 中启动扩展；TUI 切到待机会话后可由 Web SDK 打开原会话。直接退出 TUI 会结束整个进程 | `dist/modes/interactive/interactive-mode.js` 与下方实测 |
| 会话切换拦截 | `session_before_switch` 可阻止切换；fork 和 tree 也有取消事件。只有 switch 事件可能带 `targetSessionFile`，这些事件不能阻止 TUI 编辑器里的全部内置命令 | `dist/core/extensions/types.d.ts`、`dist/modes/interactive/interactive-mode.js` |
| 设置与凭据共享 | 设置写入在文件锁内重读当前文件，只写本实例改过的字段，嵌套字段按 key 合并；`AuthStorage` 按文件 revision 懒加载 | `dist/core/settings-manager.js`、`dist/core/auth-storage.js` |
| RPC 通道 | stdin/stdout 接收命令、输出事件；支持结构化选择、确认和输入，不支持任意 `ctx.ui.custom()` 组件传送 | `dist/modes/rpc/rpc-mode.js` |
| 会话恢复 | `SessionManager.open()` 与 SDK 可恢复持久化会话；恢复记录不等于恢复正在执行的请求 | `dist/core/session-manager.js`、`dist/core/sdk.js` |

源码路径均相对 `agent/node_modules/@earendil-works/pi-coding-agent/`。

## 内置命令与可用 API

TUI 的内置命令只存在于 `interactive-mode` 的编辑器提交回调里。`AgentSession.prompt()` 不认这些字符串：`/new`、`/settings` 在 SDK 与 RPC 下会被当作普通消息发给模型。Web 要覆盖它们，只能逐条调用对应 API，不能共用一条消息通道。

有两种身份，能拿到的东西不同。

| 身份 | 说明 |
|---|---|
| 宿主 | 在同进程用 SDK 建会话；`runtimeHost.services` 暴露 live 的 `settingsManager`、`modelRuntime`、`resourceLoader` |
| 扩展 | 只用 `ExtensionAPI` / `ExtensionContext` / `ExtensionCommandContext` |

| 命令或操作 | TUI 行为 | 可用 API | 宿主 | 扩展 |
|---|---|---|---|---|
| `/new` | `runtimeHost.newSession()` | `AgentSessionRuntime.newSession` | A | B |
| `/resume` | `SessionManager.list` + `runtimeHost.switchSession` | 同上，加 `SessionManager.open` | A | B |
| 会话重命名 | `session.setSessionName` | 同名方法；扩展另有 `pi.setSessionName` | A | A |
| fork | `runtimeHost.fork(entryId)` | 同名方法 | A | B |
| clone | `runtimeHost.fork(leafId, {position: "at"})` | 同名方法 | A | B |
| `/tree` | `session.navigateTree` | 同名方法，加 `sessionManager.getTree` | A | B |
| `/compact` | `session.compact(instructions)` | 同名方法；扩展另有 `ctx.compact` | A | A |
| `/reload` | `session.reload` | 同名方法 | A | B |
| `/model` | `session.setModel` + 持久化默认模型 | `session.setModel`、`settingsManager.setDefaultModelAndProvider` | A | B |
| `/scoped-models` | `session.setScopedModels` + `settingsManager.setEnabledModels` | 同名方法 | A | B |
| 模型循环 `Ctrl+P` | `session.cycleModel` | 同名方法 | A | C |
| 思考强度 | `session.setThinkingLevel` / `cycleThinkingLevel` | 同名方法；扩展另有 `pi.getThinkingLevel` / `setThinkingLevel` | A | A |
| `/settings` | 各 setter 直接改 `settingsManager` | `SettingsManager` 全部 setter | A | B |
| `/trust` | `ProjectTrustStore.setMany` | 同名方法 | A | B |
| `/login` | `ModelRuntime.login` | `ModelRuntime.login(providerId, type, interaction)` | A | B |
| `/logout` | `ModelRuntime.logout` | 同名方法 | A | B |
| `!cmd` / `!!cmd` | `session.executeBash` | 同名方法；扩展的 `pi.exec` 不写会话，语义不等价 | A | B |
| `/quit` | `runtimeHost.dispose()` | 同名方法；扩展另有 `ctx.shutdown` | A | A |
| `/debug` | 写 `pi-debug.log` | 无公开 API | C | C |
| 主题、工具输出展开 | TUI 渲染状态 | Web 自建，不需要内核 API | — | — |

A = 有公开 API 可直接实现；B = 只有命令上下文或内核内部拿得到；C = 内核无语义入口，自己重写。

关键结论：**宿主身份能把 B 全部转成 A**。纯扩展身份覆盖不了设置、凭据与大部分会话生命周期操作。设置类还有一个限制：另建 `SettingsManager` 实例写盘对运行中的会话不可见，只有同进程的 live 实例立即生效。

## 扩展运行约定

Web 扩展怎么被加载、什么时间能做什么，看下表。其中重载范围、reason 与长驻资源三条决定 Web 服务能不能活过 TUI 的会话替换。

| 事项 | 结论与限制 |
|---|---|
| 扩展发现 | 支持 `extensions/*/index.ts` 及扩展目录内的 `package.json`；前端源码可同目录存放，但不得被服务端入口作为运行依赖加载 |
| 会话替换事件 | `session_shutdown` / `session_start` 在 Pi 内置会话替换时触发。**不能用作 Web 页面切换的钩子。** Web 会话挂在扩展自建的 runtime 下，不受 TUI 侧会话替换影响 |
| 重载范围 | TUI 的 `/new`、`/resume`、fork 与 import 每次都重建扩展：`teardownCurrent()` → `session_shutdown` → `session.dispose()` → 用同一个工厂建新会话 → **扩展工厂函数重跑**，工厂闭包里的东西全部新建 |
| `session_shutdown` 的 reason | `quit` \| `reload` \| `new` \| `resume` \| `fork`。只有 `quit` 是进程退出，其余四种都是会话替换 |
| 模块级状态 | jiti 用 `moduleCache: false`，模块本身不缓存，缓存的只有工厂函数，且仅在 cwd 与 generation 未变时命中。`/reload` 走 `ResourceLoader.reload()` → `clearExtensionCache()`，**模块级变量一并重置** |
| 长驻资源 | 会话级的资源在会话开始或需要资源的命令中启动，在关闭事件中释放。**要活过会话替换的资源不能放在扩展工厂闭包里**，必须挂进程级容器；`session_shutdown` 处理器按 reason 分支，`quit` 才释放。**不能在扩展工厂加载时无条件监听端口** |
| 命令上下文 | `ExtensionCommandContext` 才有会话控制接口。不能假定任意 HTTP 回调都能安全调用它们 |

## 发行二进制

`build:binary` 用 `bun build --compile` 产出单文件二进制。扩展在所有运行模式下都由 jiti 加载（`dist/core/extensions/loader.js`）。

| 模式 | 扩展的依赖解析 |
|---|---|
| 源码 | 正常走磁盘 |
| 二进制 | jiti 走 `virtualModules` 白名单（pi 全家桶与 typebox）且 `tryNative: false`，**编译进去的原生模块不可用** |
| 二进制，扩展自带的 `node_modules` | 纯 JS 依赖可从磁盘解析 |
| 二进制，从磁盘加载原生模块 | **可以**。实测 `sharp` 在 Bun 跑 standalone 时从磁盘加载成功 |

二进制还会把 `package.json`、`theme/`、`assets/`、`export-html/`、`photon_rs_bg.wasm` 复制到二进制旁，不是完全自包含。

## 运行承载

TUI 待机会话与 Web 使用独立的会话实例，不改 Pi 内核。`/web` 先切换 TUI，再由 Web 打开它原先保存的会话。Web 运行层按会话 ID 管理实例；多会话的服务对象装配仍需验证。

| 项 | 决定 |
|---|---|
| Pi 自带 runtime | 启动时承载 TUI 会话；接管后切换为待机会话并保持进程 |
| Web 运行层 | Web 扩展在 `pi.exe` 进程内创建，按会话 ID 管理多个独立执行实例 |
| 服务实例 | `settingsManager`、`modelRuntime`、`resourceLoader` 的数量与共享方式待装配验证，见 [Issues](../issues.md#1-承载与运行) |
| 内核改动 | 不做 |
| 新进程 | 不新增 |
| 发行链 | 不变，继续用官方 `pi.exe` |

一个 `AgentSessionRuntime` 同时承载一场会话，`newSession` / `switchSession` 会替换当前实例。Web 多会话并发需要多个执行实例，不能用单个 runtime 的切换方法实现页面导航。

共享目录的协调要求见[共享状态](../SPEC/session-runtime.md#共享状态)。Web 会话不再加载 Web 扩展自身，避免递归创建服务。

`InteractiveMode` 每次切换会话都会重新绑定扩展并覆盖外部注入的 UI 实现；TUI 存在时不要注入 Web 的 uiContext。

## 上游服务端方案

`packages/server`、`client`、`protocol` 是上游自标 experimental 的远程运行时，被排除在 npm 包与二进制之外，worker 侧只有 `read` / `write` / `bash` 三个工具，不加载扩展系统。不复用。

第一方可用的宿主接口只有两组：本地 SDK（`createAgentSession` / `createAgentSessionRuntime` / `AgentSessionRuntime`）与 `--mode rpc` 加 `RpcClient`。RPC 有 `set_model`、`compact`、`bash`、`new_session`、`fork`、`clone`、`switch_session`、`set_session_name`、`get_tree` 等，缺 `list_sessions`、`navigate_tree`、settings、trust、login、reload、scoped models。

## 版本基线

以上结论以项目安装的 0.85.1 构建产物为准。本机 pi 源码仓库已领先该版本，三处相关差异：`user_bash` 失败语义（0.85.1 下拦截抛错不会阻断本地执行）、`ModelRegistry` 缺 `stream` / `streamSimple`、RPC 的 `steer` / `follow_up` 不经过扩展 `input` 事件。写实现前重新核对。

## 最小验证

2026-09-21，Windows，Node 24.15.0，Pi 0.85.1。使用两个真实 SDK 会话与模拟模型输出，不联网，不加载项目扩展，记录仅写临时目录。

| 验证项 | 结果 |
|---|---|
| 两个会话同时处于执行中 | 通过 |
| 会话 ID 与记录文件独立 | 通过 |
| 停止 A 后 B 仍在执行 | 通过 |
| B 随后正常完成，消息归属互不混入 | 通过 |
| 同一会话拒绝没有排队方式的重复并发输入 | 通过 |

本机验证脚本：`E:\tmp\2026-09-21\cst-web-session-probe\probe.mjs`。这是 SDK 基础能力验证，覆盖范围到多会话并发为止。

2026-09-23，在 `E:\tmp\2026-09-23\cst-web-runtime-probe\` 用经 SHA256 校验的官方 Windows 0.85.1 二进制、独立 home、项目诊断扩展及本机模拟模型验证：

| 验证项 | 结果 |
|---|---|
| 二进制扩展导入 SDK、创建独立会话、加载七个诊断工具并完成模拟模型回合 | 通过 |
| 扩展经 `pi.sendUserMessage` 向原 Pi 会话提交输入 | 通过；TUI 换会话后旧扩展上下文失效，旧回调不能继续使用 |
| TUI 执行 `/web`，切到独立待机会话，Web SDK 打开原会话并完成回合 | 通过；原会话文件只由 Web 实例继续写入 |
| 待机编辑器拒绝键入 `/quit`、`/new` 和 Ctrl+D | 本次 TUI 实测通过；全量快捷键与窗口关闭行为待验证 |
| 新建但尚未收到 assistant 消息的会话文件 | Pi 尚未生成文件；预建空文件再打开会重新分配会话 ID，不能据此接管原会话 |

实测脚本与结果只存于该临时目录。窗口被直接关闭仍会结束整个 Pi 进程；完整生命周期、扩展交互和多会话管理见 [Issues](../issues.md)。

## 用量与费用字段

依据本项目安装的 `@earendil-works/pi-coding-agent` **0.85.1**，核对文件：`dist/core/extensions/types.d.ts`、`dist/core/sdk.js`，以及内嵌 `pi-ai` 的 `dist/types.d.ts`、`dist/api/openai-completions.js`。

| 字段 | 可用接口 | 边界 |
|---|---|---|
| 模型、供应商、API | `message_end` 的 assistant 消息：`model`、`provider`、`api` | 以最终消息为准 |
| 响应标识、返回模型 | `responseId`、`responseModel` | 可选字段，不保证供应商提供 |
| Token | `message.usage` 的 `input/output/cacheRead/cacheWrite/totalTokens` | 思考量 `reasoning` 可选，已含在输出中 |
| 费用 | `usage.cost` 分项与 `total` | 模型价目表估算，以网关账单为准 |
| 结束状态 | `stopReason`、`errorMessage` | 延迟响应 `deferred` 需另行设计 |
| 会话 | `ctx.sessionManager.getSessionId()`、会话名称 | 本地关联 |
| 思考强度 | `providerThinkingLevel`，或请求发起时记录当前设置 | 原生供应商档位可缺失；当前设置不能反推过去请求 |
| 请求开始时间 | `before_provider_request` 时自行记录 | 初始化失败可能发生在该事件之前，需保留未知时间 |
| 模型调用耗时 | 请求发起至 assistant `message_end` 的单调时钟差 | 需实时采集 |
| 首个输出等待 | 请求发起至首个非空 `text_delta/thinking_delta/toolcall_delta` | 需实时采集，历史缺失不补造 |
| HTTP 状态与响应头 | `after_provider_response` | 流消费前触发，不表示最终成功；不保证全部分支都提供 |

采集时的三条限制：

1. 扩展的 `before_provider_request` 只有 `payload`，`after_provider_response` 只有 `status/headers`，没有统一的请求关联 ID。SDK 下层回调有 model 参数，但此版本转发到扩展事件时没有保留。
2. 单个主 Agent 的顺序调用可由采集器生成本地调用 ID，再关联 assistant 消息；跨会话、并发或嵌套调用不能共用一个全局变量。
3. OpenAI Completions 适配器在 `onPayload` 后执行 `retryProviderRequest`，成功取得响应后才调 `onResponse`。这些钩子不能还原所有失败尝试，也不能可靠推出完整重试次数。

会话 JSONL 中的 assistant 消息已保存模型、usage、`stopReason` 和时间戳，可作为历史用量来源。调用耗时、首个输出等待与完整 HTTP 流水需要新增采集记录，它们不在标准持久化字段里。`Usage` 有初始化为零的路径，失败时的零不一定代表已确认零用量。
