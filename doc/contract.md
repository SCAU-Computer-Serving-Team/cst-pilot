# 信息收集契约

本文定义 CST Pilot 本地请求记录与远程会话遥测的字段含义、身份算法与隐私收集边界。统计口径见 [telemetry/SPEC.md](telemetry/SPEC.md)，传输协议与接口见 [telemetry/API.md](telemetry/API.md)。

## 记录类型与提交边界

| `type` | 粒度 | 保存与传输范围 |
|---|---|---|
| `request` | 一次主 Agent 逻辑模型调用 | 仅保存在本地请求日志，供仪表盘读取，不上传 |
| `session` | 一场会话实例的累计统计 | 保存本地恢复草稿，并通过双端队列上传到 CSTOA 与 `timserver_1` |

使用统计通过回答结束（`agent_settled`）、定时检查（每 30 秒）、正常关闭及启动恢复统一保存和提交。会话记录支持 `0.1` 最终记录与 `0.2` 版本化累计快照；同一实例的多次快照具有相同 `recordId` 与递增 `revision`，服务端仅保留最新有效快照，不重复累加会话数、模型用量或工具调用。

计数为非负整数，耗时单位为毫秒。缺失信息使用 `null` 或省略可选对象，禁止将未知值补成 `0`。兼容读取时允许新增字段缺失。

## 在线状态边界

心跳仅报告安装与环境标识、进程级运行实例 `runId`、递增序号 `sequence`、活动状态（`idle` / `busy` / `closed`）、最后活动时刻与当前快照版本数组，不包含对话或工具正文。

`runId` 在每次进程启动时随机生成，仅用于区分并行进程与重启。接收端按服务端接收时间判断在线（90 秒超时标记失联）；失联仅表示当前不可联，不推断程序崩溃或会话结束，也不推进会话时长或产生用量。

## 公共与信封字段

### 记录公共字段

| 字段 | 含义 | 示例 |
|---|---|---|
| `v` | 记录协议版本 | `"0.1"`、`"0.2"` |
| `type` | 记录类型 | `"request"`、`"session"` |
| `recordId` | 记录唯一标识，接收端去重主键 | UUID |
| `kitVersion` | 工具包发行版本；读取失败为空字符串 | `"v0.5"` |
| `revision` | `0.2` 会话快照的递增正整数版本 | `2` |
| `final` | `0.2` 会话快照是否为实例最终关闭状态 | `false` |

### 上传信封字段

| 字段 | 含义 | 示例 |
|---|---|---|
| `v` | 上传信封协议版本 | `"0.1"`、`"0.2"` |
| `batchId` | 本次上传批次 UUID，重发可更换 | UUID |
| `sentAt` | 客户端发送时刻（ISO 8601），仅作参考 | `"2026-10-15T09:00:00+08:00"` |
| `records` | 会话记录数组，每批最多 50 条、请求体最多 1 MiB；仅 `0.2` 纯心跳允许空数组 | `[...]` |
| `heartbeat` | `0.2` 可选的当前进程运行状态对象 | `{ "runId": "...", "sequence": 1, "status": "idle" }` |

凭据与签名仅放在 HTTP 请求头，不进入信封或记录正文。

## 身份来源

### 客户端匿名标识

| 字段 | 含义 | 取值 |
|---|---|---|
| `identity.installationId` | U 盘工具包初始化生成的匿名安装标识 | UUID 或 `null` |
| `identity.environmentId` | 当前工具包密钥下的操作系统环境摘要 | 64 位小写十六进制或 `null` |
| `identity.environmentSource` | 环境标识来源 | `"windows-machine-guid"`、`"unavailable"` |

`installationId` 与 32 字节随机密钥 `identityKey` 在首次运行时写入 `agent/home/telemetry/identity.json`。`identityKey` 仅留本地，不上传。环境标识通过 HMAC-SHA256 派生，原始 `MachineGuid` 不落盘、不上传：

```text
message = UTF-8(JSON.stringify(["environment", MachineGuid.trim().toLowerCase()]))
environmentId = HMAC-SHA256(identityKey, message)
```

| 使用场景 | 标识行为与边界 |
|---|---|
| 换电脑使用同一 U 盘 | `installationId` 保持不变，`environmentId` 随目标系统改变 |
| 同一系统切换账户或提权 | `environmentId` 保持不变；不采集账户 SID |
| Windows PE 或来源不可靠 | `environmentId` 为 `null`，`environmentSource` 为 `"unavailable"` |
| 不同工具包访问同一电脑 | `identityKey` 不同，生成的 `environmentId` 不同，不可跨工具包关联电脑 |
| 复制已初始化的工具包目录 | 副本继承同一 `installationId` 与密钥，服务端无法区分原件与副本 |
| 首次运行介质只读无法落盘 | 标识保持 `null`，不生成一次性临时安装标识 |

会话开始时固定 `identity` 与环境字段，后续快照、跨机恢复与补发均保持原值。

### 服务端补充字段

| 字段 | 含义 | 取值 |
|---|---|---|
| `mid` | 本次上传凭据经 OA 内省确认的队员学号／编号；安装签名上传时为 `null` | `"20230001"` 或 `null` |
| `deviceId` | OA 凭据绑定的登录设备标识（`device_id`） | `"dev_..."` 或 `null` |
| `receivedAt` | 服务端首次接收时刻（UTC ISO 8601） | `"2026-10-15T01:00:00Z"` |
| `ip` | 上传连接的来源 IPv4 `/24` 网段 | `"203.0.113.0/24"` |

客户端提交的同名字段由服务端覆盖清除。`mid` 与 `deviceId` 仅代表本次上传请求的凭据持有人，不回填覆盖历史匿名记录的归属。

## 环境字段

| 字段 | 含义 | 示例 |
|---|---|---|
| `os.platform` | 操作系统平台 | `"win32"` |
| `os.name` | 系统版本名称 | `"Windows 11 Pro"` 或 `null` |
| `os.version` | 系统内核版本（`os.release()`） | `"10.0.26100"` |
| `os.displayVersion` | Windows 功能版本 | `"24H2"` 或 `null` |
| `os.arch` | 独立探测的操作系统原生架构 | `"x64"`、`"arm64"` 或 `null` |
| `os.isPE` | 是否运行于 Windows PE | `true`、`false`、`null` |
| `runtime.version` | Node 运行时版本 | `"24.15.0"` |
| `runtime.arch` | 运行时进程架构（`process.arch`） | `"x64"` |
| `admin` | 当前进程是否具有管理员权限 | `true`、`false`、`null` |

## 请求与模型用量字段

本地请求记录包含 `sessionId`、会话内递增序号 `seq`、发起时刻 `requestedAt`、模型响应耗时 `durationMs`（不含工具执行）、首个非空增量等待 `ttftMs`、最终状态 `status`（`"ok"` / `"aborted"` / `"error"`）、HTTP 错误码 `errorCode`、结束原因 `stopReason`，以及下表模型字段。会话记录的 `models[]` 元素共用下表模型与用量字段：

| 字段 | 含义 | 示例 |
|---|---|---|
| `provider` | 模型服务标识 | `"opencode-go"`、`"cstoa"` |
| `model` | 实际模型 ID | `"deepseek-v4.1-flash"` |
| `thinkingLevel` | 请求发起时的思考强度；未知为空字符串 | `"off"`、`"low"`、`"medium"`、`"high"` |
| `input` / `output` | 未命中缓存的输入 token / 输出 token（含思考） | `8432` / `1286` |
| `cacheRead` / `cacheWrite` | 缓存读取 token / 缓存写入 token | `32768` / `0` |
| `totalTokens` | 供应商返回的 token 总量 | `42486` |
| `cost` | Pi 价目表估算费用；会话记录中 `cstoa` 固定为 `null` | `0.2438`、`null` |
| `currency` | 与 `cost` 对应的币种（来自 `currency.json`）；未知为空字符串 | `"CNY"`、`"USD"` |

不同币种分别汇总，不做汇率换算。CSTOA 真实消费以请求级账单查询返回的额度点（`cstoaBilling`）为准，与估算费用独立统计。

## 工具采集字段

会话记录的 `tools[]` 按实际调用的 `name + scope` 聚合（无 `scope` 的工具仅按 `name` 聚合）：

| 字段 | 含义 |
|---|---|
| `name` / `scope` | 工具名与子功能名（省略参数时取默认 `scope`） |
| `calls` / `failures` | 已结束调用次数 / `isError=true` 次数 |
| `degraded` / `truncated` | 结果标记 `degraded`（含 `disk usage` 降级 `node-walk`）次数 / 标记输出裁剪次数 |
| `totalMs` / `maxMs` | 执行耗时总和 / 单次最大耗时（毫秒） |
| `resultBytes` | 结果文本 UTF-8 字节数累计 |
| `entriesTotal` | `runbook` 交付的命令条目总数 |
| `providers` / `modes` | `web_search` 服务分布对象 / `fetch_content` 模式分布对象 |

## 不采集清单

以下内容禁止作为遥测字段上传：

- 用户输入正文、系统提示词、模型回复正文、思考内容
- 工具调用的业务参数（文件路径、搜索词、URL、命令正文、BitLocker 恢复密码）与工具返回正文
- 会话标题、机主计算机名、Windows 用户名、账户 SID、原始 `MachineGuid`
- 模型 APIKEY、OAuth 令牌、网关请求 ID（`x-oneapi-request-id` 仅存本地草稿用于查账）
- 硬件序列号、U 盘序列号、MAC 地址、具体 CPU／内存／显卡型号与磁盘文件列表

**唯一正文例外**：会话记录的 `errors[].message` 保留最多 10 组高频模型请求错误原文（不截断、不上传前脱敏），服务端在接收 180 天后自动清除错误原文。
