# 会话记录字段

会话快照是一份截至指定时刻的实例累计统计，`type="session"`。公共字段、身份算法、环境字段、模型用量和工具元素以[信息收集契约](../contract.md)为准，本页补充会话结构与统计口径。

累计快照协议版本为 `0.2`，已发布的 `0.1` 最终记录继续接收和读取。读取时允许 `identity`、扩展环境字段、`runtime` 及新增扣点字段缺失；未知值保持未知，`admin` 支持 `null`。

## 统计与提交

同一实例在回答结束或定时心跳发现统计变化时提交累计快照，关闭时提交最终快照，后续提问继续使用该实例。模型、工具、环境、上下文、错误和扣点全部采用同一提交边界；中间轮次仍保存恢复草稿。

多次快照只计一个会话实例，汇总使用该实例最新有效快照；重传不得重复计数。触发与关闭路径见[发送端](sender/SPEC.md#提交触发)，快照标识与更新协议见[版本化提交](#版本化提交)。

心跳在线状态按进程运行实例保存，与会话统计分别管理；纯心跳不创建统计记录、不递增快照版本，也不推进 `endedAt`、`durationMs` 或 `activeMs`。字段与隐私边界见[契约](../contract.md#在线状态边界)，协议见下节。

## 版本化提交

`0.2` 同一实例保持 `recordId`，`revision` 从 1 递增，`final` 表示是否最终关闭。每个版本的内容固定；更高版本替换累计统计，重复或较低版本不改数据，最终版本后不再更新。

1. 所有 `0.2` 上传须验证安装签名，包含 OA 凭据时另带 `X-Telemetry-Installation` 指明签名安装。首次保存后绑定该安装，后续更新保持首次上传者、接收时间、实例与环境字段；其他安装不能更新。
2. `GET` 已配置的上传 URL 返回能力：`{ "protocols": ["0.1", "0.2"], "capabilities": ["session-snapshots", "revision-acks", "heartbeat"] }`。逐端确认能力后发送 `0.2`；能力缺失时保留新快照，不降级为 `0.1`。
3. `0.2` 信封为 `{ v, batchId, sentAt, records, heartbeat? }`。最多 50 条记录；只有心跳时允许 `records=[]`。统计和心跳分别确认。
4. `202` 回应包含 `v="0.2"`、`acks=[{ recordId, revision, status, reason? }]`。`status` 为 `stored`、`duplicate`、`stale` 或 `rejected`；仅前三项确认已持久化或被更新版本覆盖。永久拒收需逐项明确原因；`unknown-version`、`unsupported` 保留队列。缺少、格式错误或身份不匹配的确认保留对应快照。
5. 队列按 `recordId` 保留最新版本。删除确认时同时匹配实际发送的内容，旧确认不能删除后续版本。`0.1` 记录只按原协议补发，不转为可更新快照。
6. 每日汇总以首次服务端接收时间的 +08:00 日期归属，后续更新保持该日。实际使用日拆分需单独增加按日数据；当前协议不推算拆分。

### 心跳信封

`heartbeat` 包含 `runId`、`sequence`、`sentAt`、`status`（`idle`、`busy`、`closed`）、`identity`、`lastActivityAt`（可空）和 `snapshots`（`recordId`、`revision` 数组，最多 512 项）。运行 ID 每次进程启动生成，序号为递增安全整数，纯心跳不进入持久待发队列。

接收端以已验证的签名安装绑定运行 ID，仅接受比已保存序号大的心跳；客户端时间超过 90 秒或未来超过 30 秒时拒绝在线更新。有效心跳按服务端接收时间延长在线期限，关闭状态不可重新变为在线。回应 `heartbeatAck={ runId, sequence, status, reason? }`，状态为 `stored`、`duplicate`、`stale` 或 `rejected`；拒收附原因，统计确认独立处理。

每次运行只保存最新状态、最后接收时间和最后状态变化时间，不保存逐次心跳正文。最后接收超过 90 天的运行状态删除，备份遵循同一保留期。

## 会话与时间

| 字段 | 含义与来源 | 取值举例 |
|---|---|---|
| `v`、`type`、`recordId`、`kitVersion` | [公共字段](../contract.md#记录自带)，重传保持记录 ID | `"0.1"`、`"session"`、UUID、`"0.3.1"` |
| `sessionId` | Pi 会话 ID；同一会话的不同实例可产生不同 `recordId` | UUID |
| `channel` | 实际运行界面 | `"tui"`、`"web"` |
| `reason` | 实例开始原因，来自 `session_start` | `"startup"`、`"new"`、`"resume"`、`"fork"`、`"reload"` |
| `endReason` | 活跃实例快照为空字符串；最终快照填写实例结束原因，Web 删除用 `delete`，草稿恢复用 `crash` | `"quit"`、`"new"`、`"resume"`、`"fork"`、`"reload"`、`"delete"`、`"crash"` |
| `startedAt` | 实例开始时刻，带本地时区偏移 | `"2026-10-15T09:00:00+08:00"` |
| `endedAt` | 本次快照的统计截止时刻；最终快照取实例结束时刻，恢复取最后已保存活动时刻；不包含查账等待 | `"2026-10-15T09:00:10+08:00"` |
| `durationMs` | `endedAt - startedAt`，包含等待输入的空档 | `10000` |
| `activeMs` | 各 `turn_start` 至 `turn_end` 的时间累计，表示 Agent 运行时长 | `4800` |

TUI 与 Web 的原因取值见[发送端会话规则](sender/SPEC.md#会话与草稿)。Web 切换页面不结束实例；活动时长不代表队员投入时间。

## 身份与环境

| 字段或对象 | 会话中的含义 | 取值举例 |
|---|---|---|
| `identity.installationId` | U 盘工具包配置的匿名使用来源，不能直接计为一个人 | UUID 或 `null` |
| `identity.environmentId` | 同一工具包下的系统安装环境，不能直接计为物理电脑 | 64 位十六进制摘要或 `null` |
| `identity.environmentSource` | 环境标识来源；不采账户 SID | `"windows-machine-guid"`、`"unavailable"` |
| `os` | 当次运行系统的平台、名称、内核版本、功能版本、原生架构及 PE 状态 | `{ "platform": "win32", "name": "Windows 11 Pro", "version": "10.0.26100", "displayVersion": "24H2", "arch": "x64", "isPE": false }` |
| `runtime` | Node 版本与进程架构，独立于系统原生架构 | `{ "version": "24.15.0", "arch": "x64" }` |
| `admin` | 实际运行进程的管理员权限，探测失败为 `null` | `true`、`false`、`null` |

具体定义和算法见[身份来源](../contract.md#身份来源)及[环境字段](../contract.md#环境字段)。无法获取的环境属性为 `null`，不能用空白属性或相同系统名称合并用户。

身份与环境在会话开始时固定，草稿、各次快照、恢复和补发均保留原值。U 盘从电脑 A 带到 B 后上传，不以 B 的系统或权限改写 A 的记录；权限探测尚未完成时草稿可为 `null`，提交快照只能补入本实例的探测结果。

服务端补充的 `mid`、`deviceId` 表示上传时的 OA 账号与登录标识。匿名记录保留自身归属，后续账号不自动成为历史操作人；客户端提供的身份字段不能获得已验证状态。未登录时使用安装签名上传，不依赖 CSTOA；相同记录重复上传不更新首次保存的身份或环境。

`runtime.arch` 缺失时，不据 `os.arch` 推断系统原生架构；缺失的新增字段保持未知。

## 用量与模型

| 字段 | 含义与来源 | 取值举例 |
|---|---|---|
| `prompts` | `input` 来源为 `interactive` 或 `rpc` 的次数；自动重试不重复计提问 | `9` |
| `turns` | `turn_end` 次数 | `23` |
| `contextEntries` | 本次提交时上下文条目数；草稿恢复时缺失 | `47` |
| `models` | 按 `provider + model + thinkingLevel` 聚合的数组，最多 32 项 | `[{ "provider": "opencode-go", "model": "deepseek-v4.1-flash", "thinkingLevel": "high", "turns": 1, "input": 5, "output": 3, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 8, "cost": 0, "currency": "USD" }]` |

`models[].turns` 表示该组合的往返次数。其余元素字段见[模型与用量](../contract.md#模型与用量)，token 与估算费用取每轮助手消息的 usage 后累计；思考强度取请求发起时的值。CSTOA 的 `cost=null`；其他模型缺少有效费用或币种时也计入未定价。

| 会话模型字段 | 含义 | 示例 |
|---|---|---|
| `models[].incompleteUsageTurns` | 缺少 usage、totalTokens 为零，或以 error／aborted 结束的轮次数；缺失字段表示未知 | `1` |

该标记采用保守口径，保留已见 token。未标记轮次也不保证网关最终计费与 SDK usage 完全一致。

## CSTOA 扣点

`cstoaBilling` 只统计当前实例中观察到的 CSTOA 推理 HTTP 尝试，按网关请求 ID 查询已归属本人的消费账单。无观测请求时省略整个对象；计数与额度点均为非负安全整数。

| 字段 | 含义 | 示例 |
|---|---|---|
| `cstoaBilling.requests` | 观察到的请求数；相同有效请求 ID 只计一次，无 ID 的尝试分别计数 | `3` |
| `cstoaBilling.settledRequests` | 已取得有效账单扣点的请求数，不能超过 requests | `2` |
| `cstoaBilling.quotaPoints` | 已确认请求的额度点之和；没有已确认账单时为 null | `198`、`0`、`null` |

`2/3` 表示只确认了部分请求。此时 198 点是已确认部分，不能作为完整会话消耗；真实零扣点需有已确认账单。缺少请求 ID、账单延迟、网络失败、中断或无有效查询凭据均保留未知。

每次 HTTP 重试单独观测。请求 ID 仅保存在本地草稿，不上传。已入队快照保持原内容，迟到账单可在后续提交边界形成更新快照；实例最终关闭后不回填。查询不使用账号余额差，不受其他会话消费或充值差额归因影响；未观测到的请求不在覆盖范围内。

额度点独立于 `models[].cost` 与币种，不换算人民币。查询接口及有界等待见[发送端](sender/SPEC.md#cstoa-账单查询)。

## 上下文

| 字段 | 含义与来源 | 取值举例 |
|---|---|---|
| `compactions` | 成功压缩次数，来自 `session_compact` | `2` |
| `compactionTokens` | 各次成功压缩前 `tokensBefore` 之和，表示压缩前规模 | `182000` |
| `compactionOverflows` | 因 `overflow` 触发的成功压缩次数 | `1` |
| `compactionFailures` | `session_compact_failed` 次数，不采其错误正文 | `0` |
| `contextPeak` | 已取得的上下文 token 最大值；无有效采样时为 0 | `38100` |
| `contextWindow` | 达到上述峰值时对应的模型窗口大小，与峰值成对保存 | `200000` |

上下文条目数包含摘要与自定义条目。`contextPeak` 和 `contextWindow` 同时为 0 表示没有有效采样，不据此认定上下文为空。

## 工具

| 字段 | 含义与来源 | 取值举例 |
|---|---|---|
| `tools` | 按工具名与 scope 聚合的数组，最多 512 项 | `[{ "name": "disk", "scope": "usage", "calls": 2, "failures": 0, "degraded": 1, "totalMs": 41200, "maxMs": 38000, "resultBytes": 21000, "truncated": 0 }]` |
| `toolFailures` | `tool_execution_end` 中 `isError=true` 的总次数 | `1` |

元素字段见[工具采集](../contract.md#工具采集)。工具调用 ID 仅用于本地配对，不上传；结果只统计体积、状态和规定分类，不上传正文。

## 失败

| 字段 | 含义与来源 | 取值举例 |
|---|---|---|
| `providerErrors` | `after_provider_response` 中非 2xx 状态码的次数 | `{ "429": 1, "503": 2 }` |
| `networkErrors` | 轮次以 error 结束且本轮没有供应商响应的次数 | `2` |
| `aborted` | `stopReason="aborted"` 的轮次数 | `1` |

`networkErrors` 按是否收到响应计数，不能单凭该值判断具体网络故障。`providerErrors` 只统计 SDK 事件可见的响应；内部 HTTP 重试由 `cstoaBilling.requests` 单独观察，不保证全部反映在错误计数中。

## 报错原文

| 字段 | 含义与来源 | 取值举例 |
|---|---|---|
| `errors[].message` | 轮次结束时的模型请求 `errorMessage` 完整文本 | `"fetch failed: ETIMEDOUT"` |
| `errors[].count` | 完全相同文本的出现次数 | `2` |

按原文分组，次数降序保留最多 10 组，每组选中的原文不截断、不脱敏。该上限不代表保留所有不同报错；超过记录体积限额时按[发送端规则](sender/SPEC.md#队列与发送)处理，不裁剪文本。

不收工具错误正文。报错文本可能回显敏感片段，收集边界见[不采集](../contract.md#不采集)；服务端 180 天后清除原文。

## 示例

以下为带匿名标识的会话结构，上传者字段由接收端另行补充。

```json
{
  "v": "0.1",
  "type": "session",
  "recordId": "31de2e2e-816e-4cdc-8f35-c8eae6e69272",
  "kitVersion": "0.3.1",
  "sessionId": "69194984-ceb2-4abd-849c-90c85da1051b",
  "identity": {
    "installationId": "df79039e-3744-44c8-9754-7a9568ae5684",
    "environmentId": "7b28539aed99a4a8b9c422d017d9838e8c9c43d7b0ab986b4988b0808f174c12",
    "environmentSource": "windows-machine-guid"
  },
  "os": {
    "platform": "win32",
    "name": "Windows 11 Pro",
    "version": "10.0.26100",
    "displayVersion": "24H2",
    "arch": "x64",
    "isPE": false
  },
  "runtime": { "version": "24.15.0", "arch": "x64" },
  "admin": true,
  "channel": "tui",
  "reason": "new",
  "endReason": "quit",
  "startedAt": "2026-10-15T09:00:00+08:00",
  "endedAt": "2026-10-15T09:00:10+08:00",
  "durationMs": 10000,
  "activeMs": 4800,
  "prompts": 1,
  "turns": 1,
  "contextEntries": 2,
  "models": [{
    "provider": "opencode-go",
    "model": "deepseek-v4.1-flash",
    "thinkingLevel": "high",
    "turns": 1,
    "input": 5,
    "output": 3,
    "cacheRead": 0,
    "cacheWrite": 0,
    "totalTokens": 8,
    "cost": 0,
    "currency": "USD",
    "incompleteUsageTurns": 0
  }],
  "compactions": 0,
  "compactionTokens": 0,
  "compactionOverflows": 0,
  "compactionFailures": 0,
  "contextPeak": 5,
  "contextWindow": 16000,
  "tools": [],
  "providerErrors": {},
  "networkErrors": 0,
  "aborted": 0,
  "toolFailures": 0,
  "errors": []
}
```
