# 发送端

状态：设计稿，契约版本 0.1。更新：2026-09-20。会话记录字段见 [../schema.md](../schema.md)，共用字段、身份来源与计费币种见 [../../contract.md](../../contract.md)，系统切块见 [../architecture.md](../architecture.md)。

## 结论

1. pi 扩展，目录 `agent/home/extensions/telemetry/`，与 `diagnostics` 平级。零 npm 依赖，只用 Node 内置模块与全局 `fetch`。
2. 采集靠 `pi.on(...)` 订阅事件，内存累计，会话结束发一条会话记录。不改现有工具代码。
3. 上报走批量信封，先写盘再发送，收到确认后删除。全程不阻塞会话、不抛错、不向队员输出。
4. 身份由上传凭据决定：发送时带上队员的 OAuth 访问令牌，接收端解析出 `mid` 与 `deviceId`。采集侧不读凭据、不算指纹。
5. 尽力上传，允许有限丢失，不为此引入可靠消息机制。

## 模块划分

| 文件 | 职责 | I/O |
|---|---|---|
| `index.ts` | 入口：注册事件订阅，串联采集、记录、上报 | 无 |
| `collect.ts` | 采集器：事件 → 内存累计状态。纯数据结构，可单测 | 无，不同步等待任何 I/O |
| `record.ts` | `session_shutdown` 时把累计状态序列化为一条记录 | 读 ctx 快照与发行版 `VERSION` |
| `credential.ts` | 被动读 `auth.json` 取上传凭据与有效期 | 磁盘 |
| `outbox.ts` | `agent/home/telemetry/pending.jsonl`：追加、读出、成功移除、超限丢最旧 | 磁盘 |
| `transport.ts` | 批量 POST、超时、结果分类 | 网络 |
| `config.ts` | `telemetry.json` 与 `currency.json` 读取 | 磁盘 |

## 采集

采集器三条纪律：只改内存（微秒级返回）；不同步等待网络与磁盘；整体 try/catch，任何一步出错跳过本次累计。

| 事件 | 累计内容 |
|---|---|
| `session_start` | `sessionId`、`reason`、`startedAt` |
| `input` | `source = interactive` 时 `prompts++` |
| `turn_start` / `turn_end` | 配对求间隔累加 `activeMs`；`turns++`；`message.usage` 按 provider + model + thinkingLevel 分组累加进 `models`；`ctx.getContextUsage()` 采样保留峰值 `contextPeak`；`stopReason = aborted` 计 `aborted`；`stopReason = error` 时：本往返无 `after_provider_response` 则 `networkErrors++`，`errorMessage` 原文按文本分组进 `errors` |
| `after_provider_response` | 非 2xx 状态码计 `providerErrors`；标记本往返有响应（供 `networkErrors` 判定） |
| `tool_execution_start` / `_end` | `toolCallId` 配对：`tools` 公共维度（`calls`/`failures`/`totalMs`/`maxMs`/`resultBytes`/`truncated`）；扩展字段：`runbook` 取结果的 `runbook.items` 累加 `entriesTotal`、`web_search` 按 provider 计 `providers`、`fetch_content` 按 mode 计 `modes`；`details.degraded === true` 计 `degraded` |
| `tool_call` | 从入参提取 `scope`（无 scope 工具省略） |
| `session_compact` / `_failed` | `compactions`、`tokensBefore` 累加 |
| `session_shutdown` | `endedAt`、`endReason`；`messages = buildContextEntries().length`；序列化、写 outbox、触发上报（不等待） |

并行工具调用会让 start/end 交错，配对靠 `toolCallId`。

约定：

- 契约外工具只采公共维度，新增工具不改契约（见 [../../contract.md](../../contract.md)「工具采集」）。
- 参数值与输出正文不采。
- 报错原文完整入记录，不截断。同文本只留一份并按 `count` 累加，避免重复撑大记录。
- 时间序列化用带本地时区偏移的 ISO 8601（`new Date().toISOString()` 是 UTC，不符合契约）。
- `cost` 与 `currency` 一起写入：币种查 `currency.json`，查不到就留空，不猜。

### 从结果里取值的三个坑

三处都容易照着字段名猜错，已按现有代码核实：

| 字段 | 取法 | 猜错会怎样 |
|---|---|---|
| `degraded` | `details.degraded === true` | 正常成功的结果也带 `notice`（字段导读、口径说明，`driver` 每次调用都加）。按 `notice` 出现与否统计，会把大量正常调用记成降级 |
| `truncated` | A 诊断工具：结果文本含 `outputTruncated`。`diagnosticResult` 超限时只改文本，`details` 保持原对象，所以用一次子串扫描判断，不解析 JSON。B 内置 `bash`：`details.truncation.truncated` | `details.truncation` 对诊断工具不存在；`read`/`ls`/`find`/`grep` 的裁剪只写在文本里，没有结构标记，不计数 |
| `entriesTotal` | 取结果的 `runbook.items`，工具已给出命令条数 | 入参字段名是 `items`（元素为 `{ summary, command, shell, admin? }`），没有 `commands` |

## 上报

### 信封与端点

```
POST /v1/sessions
Authorization: Bearer <OA 访问令牌>
{ "v": "0.1", "batchId": "<uuid>", "sentAt": "<ISO 8601>", "records": [ ... ] }
```

单批上限 50 条或 1 MB。鉴权细节由接收端 [议题 R4](../receiver/SPEC.md) 定，发送端只负责把令牌放进请求头。

### 结果分类

| 响应 | 行为 |
|---|---|
| 202 | 批次送达，从 `pending.jsonl` 移除 |
| 401 / 403 | 凭据无效或过期。保留记录，停止本轮发送，等队员重新登录后由下一次 `session_start` 补发 |
| 400 / 413 | 这一批本身不合法。丢弃该批，避免坏批反复堵住队列 |
| 429 / 5xx / 网络错误 / 超时 | 保留，等下次 |
| 404 / 410 | 端点不再接受上报，视为停采指令。停止发送，记录保留不删 |

发送中超时 5 秒。

### 队列

`pending.jsonl` 是尽力而为的缓存，不承诺「文件里没有就一定送达过」，也不承诺「文件里的记录一定会送达」。

1. `session_shutdown`：序列化后的记录同步追加写入 `pending.jsonl`，随后触发一次发送，不等结果。
2. 发送：读出全部待发记录，组批发送，确认后从文件移除（读全、过滤、重写）。
3. `session_start` 再触发一次，清理上次遗留。
4. 文件上限 200 条，超限丢最旧。

会丢记录的情况都已知并接受：超限淘汰；磁盘只读或写入失败；进程在 `session_shutdown` 前被终止；pi 退出时最后一次发送尚未返回。为消除这些情况而引入确认与重放机制，成本高于收益。

## 身份与凭据

**待 OAuth 落地。** 本节等 OAuth 实现完成后再定稿，以下为暂定。

记录里不带任何身份字段，队员归属由接收端从上传凭据解析。

| 项 | 取法 |
|---|---|
| 上传凭据 | 被动读 `agent/home/auth.json` 中团队 OAuth provider 条目的 `access` 与 `expires`。只解析文件，不联网、不刷新、不写回 |
| 无凭据或已过期 | 不发送，记录留在队列，队员重新登录后补发 |

不在采集与上报路径上调用会触发刷新的鉴权接口。那类调用会为算一个标识而联网并写盘，把采集变成会影响正常认证的动作。令牌本身也会轮换，任何派生自令牌的标识都不稳定，所以身份一律交给接收端从凭据本身解析。

## 降级

| 情况 | 处理 |
|---|---|
| 无网络、超时、5xx、429 | 记录留在盘上，下次重试 |
| 令牌无效或过期 | 不发送，记录留住，等重新登录 |
| 无权限或设备被吊销 | 停止发送，不删数据 |
| 端点返回 404 / 410 | 端点不再接受上报，停止发送，不删数据。停采信号只借状态码传达，见 [../receiver/SPEC.md](../receiver/SPEC.md) 议题 R10 |
| 磁盘只读或空间不足 | 不写盘，记录留内存照常发送；发送也失败则丢弃 |
| 功能关闭 | 不采集、不落盘、不联网 |

以上均不提示队员。

## 配置

`agent/home/telemetry.json`，随发行写入：

```json
{ "enabled": true, "endpoint": "https://…", "authProvider": "cstoa" }
```

独立文件不用 `settings.json`：pi 未向扩展暴露设置管理器，自定义字段易受校验与迁移影响。

计费币种表随扩展放在同目录 `currency.json`，登记以人民币计价的 provider，缺省 USD，定义见 [契约](../../contract.md)「计费币种」。

源码常量：`POST_TIMEOUT_MS = 5000`、`BATCH_MAX_RECORDS = 50`、`OUTBOX_MAX_RECORDS = 200`、`ERRORS_MAX_GROUPS = 10`。

## 联网前提

`pi.cmd` 的 `PI_OFFLINE=1` 只关 pi 自身启动联网，不拦扩展请求（已在 0.85.1 的 dist 核对）。`settings.json` 的 `enableInstallTelemetry: false` 关的是 pi 自带遥测，与本扩展无关。

发行改动（待做）：`pack/pack.mjs` 的 `REPO_HOME_FILES` 白名单目前是 `APPEND_SYSTEM.md`、`models-store.json`、`open-tui.json`，`telemetry.json` 不在其中，需要加进白名单，或像 `settings.json` 一样在打包时生成。`agent/home/extensions` 整目录复制，扩展代码与 `currency.json` 不用改白名单。