# 遥测数据契约（会话记录）

状态：发送端与接收端的共同契约，契约版本 0.1。更新：2026-09-20。

本文只定义一个东西：会话记录有哪些字段、每个字段的统计口径是什么。共用字段、身份来源、计费币种、工具字段形状、不采集清单都写在 [信息收集契约](../contract.md)，本文引用不重抄。

## 会话记录字段

### 会话

| 键 | 值示例 | 来源 | 用途描述 |
|---|---|---|---|
| `sessionId` | `uuid`，pi 生成 | `ctx.sessionManager` | 与本地会话文件对应，便于回查。记录里只留前 8 位 |
| `reason` | `startup｜new｜resume｜fork` | `session_start` | 区分启动、新建、恢复、分叉 |
| `endReason` | `quit｜reload｜new｜resume｜fork` | `session_shutdown` | 区分正常退出、切换会话、重载 |

### 时间

| 键 | 值示例 | 来源 | 用途描述 |
|---|---|---|---|
| `startedAt` | `ISO 8601 带本地时区偏移` | `session_start` | 会话开始时刻，用于时段分析与跨天归属 |
| `endedAt` | `ISO 8601 带本地时区偏移` | `session_shutdown` | 会话结束时刻 |
| `durationMs` | `2382000` | `endedAt` − `startedAt` | 会话持续时间，含队员离开的空档 |
| `activeMs` | `1502000` | 发送端累计：各 `turn_start` 到 `turn_end` 的间隔求和 | Agent 运行时间：模型与工具在跑的时间，不含等待输入。衡量工具负载，不代表队员投入 |

### 用量

| 键 | 值示例 | 来源 | 用途描述 |
|---|---|---|---|
| `prompts` | `9` | `input` 事件（`source = interactive`）计数 | 队员提问次数，使用强度。不用 `agent_start`：自动重试会重复触发，不等于一次提问 |
| `turns` | `23` | `turn_end` 计数 | 模型往返次数，与请求记录条数对照 |
| `messages` | `47` | `session_shutdown` 时 `buildContextEntries().length` | 上下文规模，辅助判断压缩是否必要。不用 `message_start` 计数：它含 toolResult 事件，语义不符 |

### 模型

`models` 是**对象数组**：一次会话用到几个「provider + model + thinkingLevel」组合，就有几个元素。请求记录中的标量字段按此键分组聚合即得。

| 键 | 值示例 | 来源 | 用途描述 |
|---|---|---|---|
| `provider` | `cstoa` | `turn_end` 的 assistant 消息 | 供应商分布 |
| `model` | `glm-5.3-flash` | 同上 | 模型使用分布 |
| `thinkingLevel` | `minimal｜low｜medium｜high｜xhigh｜max` | `turn_end` 时的 `ctx.thinkingLevel` | 思考档位对成本的影响 |
| `turns` | `23` | `turn_end` 按分组键计数 | 该组合的往返次数，占比最大者即主用模型 |
| `input` | `182000` | `message.usage` 分组累计 | 输入 token，含未命中缓存的全部输入 |
| `output` | `9400` | 同上 | 输出 token，主要计费项之一 |
| `cacheRead` | `61000` | 同上 | 命中缓存的输入 token，单价低于 `input` |
| `cacheWrite` | `0` | 同上 | 写入缓存的 token，部分 provider 计费 |
| `totalTokens` | `252400` | 同上 | 用量总量，与 `turns` 对照看单次往返规模 |
| `cost` | `2.92` | `message.usage.cost.total` 分组累计 | 费用数值，计价口径见 [信息收集契约](../contract.md)「计费币种」 |
| `currency` | `CNY｜USD` | 计费币种表 | 同上 |

元素示例：

```json
{
  "provider": "cstoa",
  "model": "glm-5.3-flash",
  "thinkingLevel": "medium",
  "turns": 23,
  "input": 182000,
  "output": 9400,
  "cacheRead": 61000,
  "cacheWrite": 0,
  "totalTokens": 252400,
  "cost": 2.92,
  "currency": "CNY"
}
```

多模型会话示例（同一会话先粗排后精排）：

```json
"models": [
  { "provider": "cstoa", "model": "glm-5.3-flash",       "thinkingLevel": "low",  "turns": 18, "input": 140000, "output": 7200, "cacheRead": 52000, "cacheWrite": 0, "totalTokens": 199200, "cost": 2.00, "currency": "CNY" },
  { "provider": "cstoa", "model": "deepseek-v4.1-flash", "thinkingLevel": "high", "turns": 5,  "input": 42000,  "output": 2200, "cacheRead": 9000,  "cacheWrite": 0, "totalTokens": 53200,  "cost": 0.92, "currency": "CNY" }
]
```

### 上下文

| 键 | 值示例 | 来源 | 用途描述 |
|---|---|---|---|
| `compactions` | `0` | `session_compact` | 压缩次数，反映长会话占比 |
| `compactionTokens` | `0` | `session_compact` 的 `tokensBefore` 累计 | 被压缩的 token 规模，压缩本身也计费 |
| `contextPeak` | `38100` | `ctx.getContextUsage()` | 上下文峰值，判断是否贴近窗口上限 |

### 工具

`tools` 是**对象数组**：会话中用到几个「name + scope」组合，就有几个元素。同一工具的不同 scope 各占一个元素，如 `sys(gpu)` 与 `sys(proc)`；无 scope 的工具按 name 分组。

字段形状、采集范围与边界见 [信息收集契约](../contract.md)「工具采集」。本节只定分组规则与上限：

| 项 | 规则 |
|---|---|
| 分组键 | `name` + `scope`，不是调用次序 |
| 元素数上限 | 512。正常会话几十个即封顶，上限仅防 scope 取值失控撑爆记录 |
| 契约外工具 | 只采公共维度，按 name 分组 |
| 元素示例 | `{ "name": "disk", "scope": "usage", "calls": 2, "failures": 0, "degraded": 1, "totalMs": 41200, "maxMs": 38000, "resultBytes": 21000, "truncated": 0 }` |

多元素示例（同一 `sys` 工具按 scope 拆分）：

```json
"tools": [
  { "name": "disk", "scope": "usage", "calls": 2, "failures": 0, "degraded": 1, "totalMs": 41200, "maxMs": 38000, "resultBytes": 21000 },
  { "name": "sys",  "scope": "gpu",   "calls": 1, "failures": 0, "degraded": 0, "totalMs": 3100,  "maxMs": 3100,  "resultBytes": 4200 },
  { "name": "sys",  "scope": "proc",  "calls": 3, "failures": 1, "degraded": 0, "totalMs": 9800,  "maxMs": 4100,  "resultBytes": 12600 }
]
```

### 失败

| 键 | 值示例 | 来源 | 用途描述 |
|---|---|---|---|
| `providerErrors` | `{ 429: 1 }` | `after_provider_response` 状态码分类计数 | 区分网关限流、故障与模型问题，可由请求记录聚合 |
| `networkErrors` | `2` | `turn_end` 的 `stopReason = error` 且本往返没有 `after_provider_response` | 网络层失败计数：断网、DNS 失败、连接超时等无响应错误 |
| `aborted` | `0` | `stopReason = aborted` 计数 | 队员主动取消次数，模型跑偏的信号 |
| `toolFailures` | `0` | `tool_execution_end` 的 `isError` 合计 | 工具失败总数，与 `tools[].failures` 对照 |

### 报错原文

`errors` 是**对象数组**：按报错文本分组，同文本累加 `count`。只收 turn 级错误（provider 错误、网络错误、运行异常），不收工具错误正文。

| 键 | 值示例 | 来源 | 用途描述 |
|---|---|---|---|
| `message` | `fetch failed: ETIMEDOUT` | `turn_end` 的 `errorMessage` | 报错原文，**完整收集、不截断、不脱敏**，见 [信息收集契约](../contract.md)「不采集」 |
| `count` | `2` | 发送端按文本分组计数 | 同文本错误重复次数 |

元素示例：

```json
{ "message": "429 rate_limit_error: max RPM reached", "count": 2 }
```

分组是为了不重复撑大记录，不是为了削减内容：同一段报错出现几次都只有一份原文。组数上限见议题 S4。

### 环境

| 键 | 值示例 | 来源 | 用途描述 |
|---|---|---|---|
| `os.version` | `10.0.26100` | 运行时 | 兼容性决策 |
| `os.arch` | `x64｜arm64` | 运行时 | 同上 |
| `admin` | `true｜false` | 运行时 | 是否管理员运行，与 `degraded` 对照解释降级原因 |

## 示例

```json
{
  "v": "0.1",
  "type": "session",
  "recordId": "0f3a91c47bd2e5a8",
  "kitVersion": "0.3.1",
  "sessionId": "1f2e8a3b",
  "reason": "startup",
  "endReason": "quit",
  "startedAt": "2026-09-16T14:02:11+08:00",
  "endedAt": "2026-09-16T14:41:53+08:00",
  "durationMs": 2382000,
  "activeMs": 1502000,
  "prompts": 9,
  "turns": 23,
  "messages": 47,
  "models": [
    {
      "provider": "cstoa",
      "model": "glm-5.3-flash",
      "thinkingLevel": "medium",
      "turns": 23,
      "input": 182000,
      "output": 9400,
      "cacheRead": 61000,
      "cacheWrite": 0,
      "totalTokens": 252400,
      "cost": 2.92,
      "currency": "CNY"
    }
  ],
  "compactions": 0,
  "compactionTokens": 0,
  "contextPeak": 38100,
  "tools": [
    { "name": "disk", "scope": "usage", "calls": 2, "failures": 0, "degraded": 1, "totalMs": 41200, "maxMs": 38000, "resultBytes": 21000 },
    { "name": "sys",  "scope": "gpu",   "calls": 1, "failures": 0, "degraded": 0, "totalMs": 3100,  "maxMs": 3100,  "resultBytes": 4200 }
  ],
  "providerErrors": { "429": 1 },
  "networkErrors": 1,
  "errors": [
    { "message": "429 rate_limit_error: max RPM reached", "count": 1 }
  ],
  "aborted": 0,
  "toolFailures": 0,
  "os": { "version": "10.0.26100", "arch": "x64" },
  "admin": false
}
```

`mid`、`deviceId`、`receivedAt`、`ip` 由接收端写入，不在上传的记录里。

## 尺寸

会话记录约 1–3 KB。`errors` 的原文可能显著撑大记录，单条最长按实际报错为准，不设截断；受影响的只是这一条记录的体积，不改变组批规则。`tools` 元素数上限 512，`models` 通常 1–3 项。
