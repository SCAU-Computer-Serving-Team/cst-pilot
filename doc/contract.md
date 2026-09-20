# 信息收集契约

状态：信息收集业务的最高参考，契约版本 0.1。更新：2026-09-20。

本文是信息收集业务的最高参考：字段定义、身份来源、计费币种、工具字段形状、隐私边界都只有这一份定义，其他文档引用不重抄；写法冲突时以本文为准。

消费方是两个：遥测系统上传会话记录，[仪表盘](design/web/dashboard.md) 只看请求记录。两种记录共存，按 `type` 区分：

| `type` | 粒度 | 产生时机 | 主要用途 |
|---|---|---|---|
| `request` | 主 Agent 每次模型调用一条 | 该次调用结束后 | 模型请求日志、逐次成本、本地仪表盘。只落本地，不上传 |
| `session` | 一场会话一条 | `session_shutdown` | 工具使用、降级、活跃时长、上下文规模。上传遥测，字段见 [telemetry/schema.md](telemetry/schema.md) |

「值示例」里的 `uuid`、`sha256(...)`、`ISO 8601` 等是格式记号，不是字面值。具体取值见文末示例。

## 记录自带

两种记录都携带。

| 键 | 值示例 | 来源 | 用途描述 |
|---|---|---|---|
| `v` | `"0.1"` | 常量 | 契约版本。接收端据此选择解析规则 |
| `type` | `request｜session` | 常量 | 记录类型，接收端据此选择解析规则 |
| `recordId` | `uuid` | 发送端生成 | 记录唯一标识，接收端去重主键 |
| `kitVersion` | `0.3.1` | 发行版 `VERSION` | 版本分布，决定旧版兼容与问题定位 |

记录不自带任何身份字段，队员归属见「身份来源」。

`v` 就是本文档的契约版本，全仓只有这一个版本号，改动时文档与记录一起改。按字符串比较，不做数值运算。

## 身份来源

身份由上传凭据决定，客户端不计算、不携带。

| 键 | 来源 | 用途描述 |
|---|---|---|
| `mid` | 上传凭据载荷 | 队员编号。使用统计与报表的归属单位 |
| `deviceId` | 上传凭据载荷的 `device_id` | 设备标识。限流与删除的单位 |
| `receivedAt` | 接收端时钟 | 时间基准，客户端时钟不可信 |
| `ip` | 接收端从连接获取 | 来源统计，处理方式见 [telemetry/receiver/SPEC.md](telemetry/receiver/SPEC.md) 议题 R6 |

上传凭据是队员登录得到的 [OA 访问令牌](auth/README.md)，与模型调用共用同一个令牌。接收端校验签名、类型、作用域、有效期与设备状态，取出 `mid` 与 `device_id` 写入记录。

这条路线有两个结果：

1. **采集侧不碰凭据。** 不调用会触发刷新与写盘的鉴权接口，不从记录里携带任何派生自凭据的标识，不做指纹计算。身份在接收端由令牌决定，与队员用哪个 provider、换不换 API KEY 都无关。
2. **令牌无效时记录留盘。** 未登录或令牌过期的会话，记录留在本地待发队列，等队员重新登录后补发，不丢弃。

自带 API KEY、未走 OA 登录的会话没有上传凭据，记录进队列但发不出去，最终被队列上限淘汰。

## 请求记录字段

### 关联

| 键 | 值示例 | 来源 | 用途描述 |
|---|---|---|---|
| `sessionId` | `uuid`，pi 生成 | `ctx.sessionManager` | 关联会话记录与本地会话文件。只记前 8 位 |
| `seq` | `17` | 发送端累计 | 会话内第几次模型调用，从 1 开始。缺口即丢失率 |

### 时间

| 键 | 值示例 | 来源 | 用途描述 |
|---|---|---|---|
| `requestedAt` | `ISO 8601 带本地时区偏移` | 发送端记录 | 调用开始时刻 |
| `durationMs` | `4820` | `turn_end` 时间戳差 | 本次请求的模型调用耗时 |
| `ttftMs` | `640` | 首个 assistant `message_update` 到达耗时 | 首字延迟，从 `turn_start` 起算 |

### 模型与用量

| 键 | 值示例 | 来源 | 用途描述 |
|---|---|---|---|
| `provider` | `cstoa` | `turn_end` 的 assistant 消息 | 本次请求的供应商 |
| `model` | `glm-5.3-flash` | 同上 | 本次请求的模型 |
| `thinkingLevel` | `minimal｜low｜medium｜high｜xhigh｜max` | `turn_end` 时的 `ctx.thinkingLevel` | 本次请求的思考档位，取值与 pi 的 `ThinkingLevel` 一致 |
| `input` | `8432` | `message.usage` | 输入 token，含未命中缓存的全部输入 |
| `output` | `1286` | 同上 | 输出 token |
| `cacheRead` | `32768` | 同上 | 命中缓存的输入 token |
| `cacheWrite` | `0` | 同上 | 写入缓存的 token |
| `totalTokens` | `42486` | 同上 | 用量总量 |
| `cost` | `0.2438` | `message.usage.cost.total` | 费用数值。计价口径与币种见「计费币种」 |
| `currency` | `CNY｜USD` | 计费币种表 | 该 provider 价目表的币种。取值规则见「计费币种」 |

### 结果

| 键 | 值示例 | 来源 | 用途描述 |
|---|---|---|---|
| `status` | `ok｜aborted｜error` | `stopReason`、provider 状态码 | 结束状态，`aborted` 为队员主动取消，`error` 为 provider 非 2xx |
| `errorCode` | `429` | `after_provider_response` | HTTP 状态码，仅 `status = error` 且收到响应时存在。网络层失败（无响应）没有状态码。区分网关限流、故障与模型问题 |
| `stopReason` | `stop｜length｜toolUse｜error｜aborted｜deferred` | `turn_end` | 模型侧结束原因，取值与 pi 的 `StopReason` 一致；`pending` 为中间态不记录。与 `status` 对照解释取消与截断 |

## 计费币种

`cost` 与 `currency` 必须同源：数值来自 provider 的价目表，币种必须是那份价目表的币种。不做汇率换算。

币种由一张单独维护的映射表决定，**缺省 `USD`**：

| provider | base URL 主机 | 币种 | 依据 |
|---|---|---|---|
| `cstoa`（团队 OA 代理） | `cstoa.top` | `CNY` | OA 网关按人民币计价 |
| 其余 pi 内置 provider | — | `USD` | pi 的价目表按美元标价 |

规则：

1. 表里只登记以人民币计价的 provider，其余一律按缺省 `USD`。
2. 新增 provider 只改这一处，不动字段定义。
3. 表里没有、又无法确认币种的 provider 不猜：`currency` 留空，报表把该条单列为「未定价」，不出现在合计里。
4. **经过国内代理不改变币种。** 按美元价目表算出的费用，不会因为流量走了国内地址就变成人民币。

登记范围见议题 S3。

## 工具采集

会话记录 `tools[]` 的字段形状以本节为准，[telemetry/schema.md](telemetry/schema.md) 只讲分组规则。

### 采集范围

只采工具包启用的工具：`settings.json` 的 `defaultTools` 内置工具，加扩展与包注册的工具。当前清单：

| 工具 | 来源 | scope |
|---|---|---|
| `read` | 内置 | 无 |
| `ls` | 诊断（覆盖内置） | 无 |
| `disk` | 诊断 | `info` / `space` / `usage` / `health` / `all` |
| `sys` | 诊断 | `proc` / `gpu` / `sensor` / `io` / `overview` |
| `startup` | 诊断 | 无（同名字段） |
| `eventlog` | 诊断 | `recent` / `query` / `bluescreen` / `boot` / `crash` |
| `driver` | 诊断 | `devices` / `problem` / `external` / `find` / `core` |
| `runbook` | 诊断 | 无 |
| `fff` | 包（pi-fff） | 无 |
| `open_tui` | 包（pi-open-tui） | 无 |
| `web_search` | 包（pi-web-access） | 无 |
| `fetch_content` | 包（pi-web-access） | 无 |
| `get_search_content` | 包（pi-web-access） | 无 |

契约外工具只采公共维度。启用新工具或装新包不需要改契约。

### 元素公共维度

| 键 | 值示例 | 来源 | 用途描述 |
|---|---|---|---|
| `name` | `disk`，工具名 | `tool_execution_end` 的入参 | 哪些工具真被使用 |
| `scope` | `usage`，随工具不同 | 同上 | 子功能分布，无 scope 的工具省略 |
| `calls` | `2` | `tool_execution_end` 计数 | 使用强度 |
| `failures` | `0` | `isError` 计数 | 工具质量的直接指标，仅 `isError` 计入 |
| `degraded` | `1` | 结果里降级标记出现次数 | 现场降级率，反映免管理员与缺驱动的影响 |
| `totalMs` | `41200` | `tool_execution_start/end` 配对 | 总耗时，定位卡顿工具 |
| `maxMs` | `38000` | 同上 | 最慢一次，判断是否有偶发长耗时 |
| `resultBytes` | `21000` | 结果文本字节数 | 输出体积是否合理，是否触及 50 KiB 裁剪线 |
| `truncated` | `1` | 结果触及输出上限的次数 | 判断输出是否过大 |

### 分工具扩展

可选，该工具出现即携带。

| 工具 | 字段 | 值示例 | 用途描述 |
|---|---|---|---|
| `runbook` | `entriesTotal` | `12` | 交付的命令条目数累计 |
| `web_search` | `providers` | `{ tavily: 5 }` | 搜索渠道分布 |
| `fetch_content` | `modes` | `{ readable: 3, raw: 1 }` | 抓取模式分布 |

### 边界

参数值与输出正文不采：搜索词、URL、文件路径、命令正文都不进记录。

## 示例

请求记录：

```json
{
  "v": "0.1",
  "type": "request",
  "recordId": "9e2b71c4d8f34a65",
  "kitVersion": "0.3.1",
  "sessionId": "1f2e8a3b",
  "seq": 17,
  "requestedAt": "2026-09-19T14:32:08+08:00",
  "provider": "cstoa",
  "model": "glm-5.3-flash",
  "thinkingLevel": "medium",
  "input": 8432,
  "output": 1286,
  "cacheRead": 32768,
  "cacheWrite": 0,
  "totalTokens": 42486,
  "cost": 0.2438,
  "currency": "CNY",
  "durationMs": 4820,
  "ttftMs": 640,
  "status": "ok",
  "stopReason": "stop"
}
```

请求记录约 300–500 字节。会话记录约 1–3 KB，尺寸约定见 [telemetry/schema.md](telemetry/schema.md)。

## 不采集

对话正文、系统提示词、模型输出、工具参数值与输出正文、文件路径原文、会话名、计算机名与用户名、模型凭据原文、硬件序列号。IP 由接收端从连接获取，客户端不参与。

**唯一例外：turn 级报错原文。** 报错正文按会话记录的 `errors` 字段完整收集，不截断、不上传前脱敏，见 [telemetry/schema.md](telemetry/schema.md)「报错原文」。

理由是这条正文是定位供应商与网络故障的唯一依据，摘要会丢掉关键片段。代价是它可能回显路径、账号或请求片段，风险由这一条例外显式承担，不扩及其他字段。

只采集本工具自身的运行信息，不做审计、监控与远程控制，也不替代网关账单。

## 能回答的问题

| 问题 | 数据 |
|---|---|
| 谁在用，用多久 | `mid`、会话起止与时长 |
| 用了哪些模型，花多少 | 请求记录的 model、token、费用；会话记录 `models` 聚合 |
| 用了哪些工具，失败几次 | 会话记录 `tools` 的调用数、失败数、耗时 |
| 单次调用卡不卡 | 请求记录的 `durationMs`、`ttftMs`、`status` |
| 从哪里接入 | 接收端记录的来源 IP |
| 什么环境，哪个版本 | `kitVersion`、`os`、`admin` |

## 议题

数据定义相关的未决项。接收端的接口与运维议题归 [telemetry/receiver/SPEC.md](telemetry/receiver/SPEC.md)。
| 组 | 编号 | 议题 | 状态与候选 |
|---|---|---|---|
| 效果指标 | S1 | 用什么指标衡量工具好不好用 | 会话内重复提问次数、队员取消次数、界面等待时长。MVP 不考虑，后续持续跟进 |
| 费用口径 | S2 | 费用在客户端算还是接收端算 | A 客户端按价目表算，接收端只存。B 接收端按自己的价目表重算。C 只存 token，报表时对网关账单 |
| 计费币种 | S3 | 币种表登记范围 | 团队的 OA 代理 provider 记 `CNY`。pi 内置的国内站点 provider（`deepseek`、`zai-coding-cn`、`moonshotai-cn`、`minimax-cn`、`qwen-token-plan-cn`、`xiaomi-token-plan-cn`）的价目表是否按人民币标价，逐条核对后再登记；对不上的保持缺省 `USD` |
| 报错原文 | S4 | 组数上限 | 单条不截断。按文本分组的组数是否保留上限（现值 10 组），超出的组丢弃算不算违背「完整收集」 |
