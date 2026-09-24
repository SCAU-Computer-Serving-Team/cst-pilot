# opencode 参考实现

状态：桌面端与浏览器端调研完毕，可借鉴项已提炼。原始报告在临时目录，不随仓库保留。更新：2026-09-22。

调研对象是 opencode 的 Electron 桌面端与浏览器端。两端共用同一份前端代码（`packages/app`），服务端是一个本地进程，浏览器形态由 `opencode web` 起在回环地址。形态与本项目相同，所以它的交互设计可以借语义，代码不能借。

## 形态对照

| 项 | opencode | 本项目 |
|---|---|---|
| 服务进程 | Electron `utilityProcess.fork` 起 sidecar，多窗口共用 | 一个 pi 进程，扩展管理 Web 会话实例 |
| 浏览器形态 | `opencode web`，回环地址加可选密码 | 扩展监听回环地址 |
| 通信 | HTTP + SSE | 见 [页面地址与接口设计](../SPEC/app-router.md#接口) |
| 前端 | SolidJS，桌面与浏览器同一份代码 | React SPA |
| 写权 | 不做仲裁，多端都能提交，服务端按会话串行 | Web 各端不仲裁；TUI 待机后由 Web 接管会话 |
| 输入 | 先落盘分配序号，再提升进模型可见历史 | 已采纳，见[输入收件箱](../SPEC/session-runtime.md#输入收件箱) |

## 收件箱分层

服务端把「收下」和「进历史」分开。输入先持久化落盘并分配序号，此时已经可查询、可幂等重试、可确认顺序。之后才在安全边界提升进模型可见历史。

| 投递方式 | 提升时机 |
|---|---|
| `steer` | 每个 provider-turn 边界都可以提升 |
| `queue` | 每个执行轮最多提升一条，且只在会话本就空闲时 |

两条不变量：

1. 同一个消息 ID 重复提交，只有在会话、内容、投递方式三者都一致时才算重试，否则报冲突
2. 提升单调，序号只写一次，用空值判断做 CAS，并发提升不会重复

出处 `packages/core/src/session/input.ts:78-160`、`runner/llm.ts:386-417`。这套分层比直接调 `run()` 稳，可供同进程多会话输入管理参考。

## 可借鉴

| 做法 | 出处 | 意义 |
|---|---|---|
| 图片按内容 hash 存浏览器 IndexedDB，用 object URL 渲染，启动时按引用计数回收孤儿 | `app/src/utils/draft-store.ts:17-34,103-152` | 不需要附件上传接口，刷新后未发送的图片还在 |
| 附件限流按「一次选择的共享字节预算」，不按单个文件 | `desktop/src/main/attachment-picker.ts:6-38` | 单张不设上限、整批设上限；预算在读取时递减 |
| 客户端队列形状：`{items, failed, paused, edit}` 四个映射加一条「队首自动发送」effect | `app/src/pages/session.tsx:606-618,1925-1941` | 一个键表达一个状态，容易用 `useReducer` 复刻 |
| 「编辑」等于出队加回填输入框，不单独做删除 | `app/src/pages/session.tsx:1797-1812` | 省掉编辑态，且内容不会同时存在于队列和输入框 |
| 停止即暂停队列 | `app/src/pages/session.tsx:2199-2204` | 用户按 Esc 的预期就是停下来 |
| 「忙且输入框为空」才把发送键变成停止键 | `app/src/components/prompt-input.tsx:286-297,1578-1590` | 忙且有内容时按钮语义是插话，不骗用户 |
| 单会话串行：同键复用执行、唤醒合并、中断即打断 owner | `core/src/session/run-coordinator.ts:37-103` | 三个方法，Promise 加 AbortController 就能实现 |
| 中断三段落地：先 flush 正文，再标记步骤失败，再标记工具失败，启动时补扫悬空工具 | `core/src/session/runner/llm.ts:119-140,304-320` | 中断不留半成品 |
| 事件流按 16 毫秒帧批量 flush，同类增量合并 | `app/src/context/server-sdk.tsx:218-257` | 减少逐事件触发的重复渲染 |
| 工具卡默认展开是一个纯函数加两个设置开关；运行中的工具不能展开 | `session-ui/src/components/part-default-open.ts` | 纯函数可测试；运行中的内容还在变 |
| 连续的 read/glob/grep/list 合并成一行带计数的摘要 | `session-ui/src/components/message-part.tsx:606-608,1043-1110` | 让长回合可读最有效的一招 |
| 正文增量追加，结束时整段替换 | `app/src/context/server-session-v2-reducer.ts:190-203` | 流式渲染与刷新后重建共用一条代码路径 |
| 思考强度取模型自带的选项列表，选中值随消息快照，没有可选项就不显示控件 | `app/src/pages/session/composer/prompt-model-selection.ts:91-135` | 不为不支持思考的模型留空控件 |
| 重内容首次展开才挂载 | `session-ui/src/components/basic-tool.tsx:130-170` | 长会话收益大 |

## 不适合照搬

| 做法 | 原因 |
|---|---|
| SolidJS 组件与 Kobalte 实现 | 技术栈不同。可迁移的是状态机形态：`transition(state, event) -> {state, commands}` 纯函数 |
| 只用实时流，靠重连后全量刷新恢复 | 本地回环下够用，但流式输出中刷新会丢中间态。它协议里已有 `after` 游标，前端没用 |
| 浏览器多标签共用同一份持久化数据 | 它没有广播通道，两个标签会互相覆盖。桌面端靠每窗口独立 id 规避 |
| 服务端截断后不给前端「查看完整输出」入口 | 完整输出写进受管临时文件，路径只给模型，前端拿不到。这是它的缺口 |
| 图片以 base64 放入请求体 | 需要限制整条消息的体积；本项目由后端持久化已接受图片，见[图片规则](../SPEC/chat-workspace.md#图片) |
| 单字符 `!` 切 shell 模式、整行只有一个斜杠词才算命令 | 浏览器里会误触发。本项目已否决 shell 透传 |
| 停止权限 | 本项目允许任一 Web 页面停止所查看会话，界面需同步显示结果，规则见[会话运行](../SPEC/session-runtime.md#写权与会话串行) |
