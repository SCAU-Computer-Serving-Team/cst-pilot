# 仪表盘

设计稿位于 `web/design/cst-pilot-web.pen` 的「仪表盘 · 浅色」（`tdc8d`）与「仪表盘 · 深色」（`v3B33`）。宽 1920px，高 1925px，保留 264px 侧栏。当前只有设计稿，数据均为示例，尚未实现采集与页面交互。

## 页面

| 区域 | 内容 |
|---|---|
| 筛选 | 供应商、模型、日期范围；默认近 14 天 |
| 概览 | Token 总量、模型调用数、USD 估算费用、缓存命中率 |
| 趋势 | 按日堆叠柱，区分输入、输出、缓存读取、缓存写入；预留费用视图 |
| 日志 | 请求时间、模型与供应商、所属会话、输入、输出、缓存读写、估算费用、耗时与首个输出等待、结束状态 |
| 操作 | 搜索、状态与会话筛选、列设置、详情入口、CSV 导出、分页；当前只表达设计意图 |

组件结构参考 HeroUI v3，圆角沿用项目已有变量：控件、标签与分页 6px，分段选择器外框 8px，卡片与表格 12px，不使用胶囊式控件。颜色来自项目色阶，中文使用思源黑体，数值与模型名使用 Inter；不改动已有页面。

## Pi 接口核对

依据本项目安装的 `@earendil-works/pi-coding-agent` **0.85.1**，不是把全局 Pi 版本当作实现依据。核对文件：`dist/core/extensions/types.d.ts`、`dist/core/sdk.js`，以及内嵌 `pi-ai` 的 `dist/types.d.ts`、`dist/api/openai-completions.js`。

**一行表示主 Agent 的一次逻辑模型调用，不是一场会话，也不是每一次 HTTP 尝试。** 工具执行不另算模型调用；压缩、分支摘要及工具内部模型调用不能假定都经过同一组事件，扩展覆盖范围时需单独接入。

| 字段 | 可用接口 | 边界 |
|---|---|---|
| 模型、供应商、API | `message_end` 的 assistant 消息：`model`、`provider`、`api` | 以最终消息为准 |
| 响应标识、返回模型 | `responseId`、`responseModel` | 可选字段，不保证供应商提供 |
| Token | `message.usage` 的 `input/output/cacheRead/cacheWrite/totalTokens` | 思考量 `reasoning` 可选，已含在输出中，不再次相加 |
| 费用 | `usage.cost` 分项与 `total` | 模型价目表估算，不是网关账单；零价格不自动表示免费 |
| 结束状态 | `stopReason`、`errorMessage` | 完成 `stop`、工具调用 `toolUse`、达到上限 `length`、失败 `error`、取消 `aborted`；延迟响应 `deferred` 需另行设计 |
| 会话 | `ctx.sessionManager.getSessionId()`、会话名称 | 本地关联，不据此把会话名纳入遥测上报 |
| 思考强度 | `providerThinkingLevel`，或请求发起时记录当前设置 | 原生供应商档位可缺失；当前设置不能反推过去请求 |
| 请求开始 | `before_provider_request` 时自行记录时间 | 初始化失败可能发生在该事件之前，需保留未知时间 |
| 模型调用耗时 | 请求发起至 assistant `message_end` 的单调时钟差 | 需实时采集；包含该逻辑调用内的等待与重试，不含随后工具执行 |
| 首个输出等待 | 请求发起至首个非空 `text_delta/thinking_delta/toolcall_delta` | 不是 HTTP 响应头耗时；需实时采集，历史缺失不补造 |
| HTTP 状态与响应头 | `after_provider_response` | 流消费前触发，不表示最终成功；不保证全部传输和失败分支都提供 |

### 关联与重试限制

1. 扩展的 `before_provider_request` 只有 `payload`，`after_provider_response` 只有 `status/headers`，没有统一的请求关联 ID。SDK 下层回调有 model 参数，但此版本转发到扩展事件时没有保留。
2. 单个主 Agent 的顺序调用可由采集器生成本地调用 ID，再关联 assistant 消息；跨会话、并发或嵌套调用不能共用一个全局变量。
3. OpenAI Completions 适配器在 `onPayload` 后执行 `retryProviderRequest`，成功取得响应后才调用 `onResponse`。这些钩子不能还原所有失败尝试，也不能可靠推出完整重试次数。
4. 不用 `turn_end - turn_start` 作为模型耗时：一个 turn 还包含模型之后的工具执行。

### 历史与缺失数据

会话 JSONL 中的 assistant 消息已保存模型、usage、stopReason 和时间戳，可作为历史用量来源。调用耗时、首个输出等待、完整 HTTP 流水并不是这些消息的标准持久化字段，需要新增采集记录。

- `Usage` 有初始化为零的路径；失败时的零不一定代表已确认零用量。采集层需区分已报告、未报告与未知。
- `—` 表示缺失；明确的零保留为 `0`。未配置有效价格显示「未定价」，不显示 `$0.00`。
- 总量按可用 usage 汇总。缓存命中率按 `cacheRead / (input + cacheRead + cacheWrite)` 计算，分母为零时不计算。
- 本页不替代 [会话遥测讨论](../../telemetry/README.md)，也不把其未实现字段当作已完成能力。
