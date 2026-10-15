# 会话记录字段

会话遥测按实例聚合，`type="session"`。公共字段、身份算法、环境字段、模型用量和工具元素以[信息收集契约](../contract.md)为准，本页补充会话结构与统计口径。

协议版本为 `0.1`。读取时允许 `identity`、扩展环境字段和 `runtime` 缺失；未知值保持未知，`admin` 支持 `null`。

## 会话与时间

| 字段 | 含义与来源 | 取值举例 |
|---|---|---|
| `v`、`type`、`recordId`、`kitVersion` | [公共字段](../contract.md#记录自带)，重传保持记录 ID | `"0.1"`、`"session"`、UUID、`"0.3.1"` |
| `sessionId` | Pi 会话 ID；同一会话的不同实例可产生不同 `recordId` | UUID |
| `channel` | 实际运行界面 | `"tui"`、`"web"` |
| `reason` | 实例开始原因，来自 `session_start` | `"startup"`、`"new"`、`"resume"`、`"fork"`、`"reload"` |
| `endReason` | 实例结束原因；Web 删除用 `delete`，草稿恢复用 `crash` | `"quit"`、`"new"`、`"resume"`、`"fork"`、`"reload"`、`"delete"`、`"crash"` |
| `startedAt` | 实例开始时刻，带本地时区偏移 | `"2026-10-15T09:00:00+08:00"` |
| `endedAt` | 正常定稿时刻；恢复时取最后完成轮次时刻 | `"2026-10-15T09:00:10+08:00"` |
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

身份与环境在会话开始时固定，草稿、定稿、恢复和补发均保留原值。U 盘从电脑 A 带到 B 后上传，不以 B 的系统或权限改写 A 的记录；权限探测尚未完成时草稿可为 `null`，定稿只能补入本实例的探测结果。

服务端补充的 `mid`、`deviceId` 表示上传时的 OA 账号与登录标识。匿名记录保留自身归属，后续账号不自动成为历史操作人；客户端提供的身份字段不能获得已验证状态。未登录时使用安装签名上传，不依赖 CSTOA；相同记录重复上传不更新首次保存的身份或环境。

`runtime.arch` 缺失时，不据 `os.arch` 推断系统原生架构；缺失的新增字段保持未知。

## 用量与模型

| 字段 | 含义与来源 | 取值举例 |
|---|---|---|
| `prompts` | `input` 来源为 `interactive` 或 `rpc` 的次数；自动重试不重复计提问 | `9` |
| `turns` | `turn_end` 次数 | `23` |
| `contextEntries` | 正常定稿时上下文条目数；草稿恢复时缺失 | `47` |
| `models` | 按 `provider + model + thinkingLevel` 聚合的数组，最多 32 项 | `[{ "provider": "opencode-go", "model": "deepseek-v4.1-flash", "thinkingLevel": "high", "turns": 1, "input": 5, "output": 3, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 8, "cost": 0, "currency": "USD" }]` |

`models[].turns` 表示该组合的往返次数。其余元素字段见[模型与用量](../contract.md#模型与用量)，token 与费用取每轮助手消息的 usage 后累计；思考强度取请求发起时的值。币种分别汇总，未知币种的费用单列为未定价。

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

`networkErrors` 按是否收到响应计数，不能单凭该值判断具体网络故障。成功重试前的供应商失败仍计入 `providerErrors`。

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
    "currency": "USD"
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
