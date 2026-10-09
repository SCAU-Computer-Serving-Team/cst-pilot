# 发送端

状态：已实现，已接入发行包，采集、队列、定稿与本机 Go 接收端联调通过。真实队员公网上传待验收。契约版本 0.1。更新：2026-10-08。会话记录字段见 [../schema.md](../schema.md)，共用字段、身份来源与计费币种见 [../../contract.md](../../contract.md)，系统切块见 [../architecture.md](../architecture.md)。

## 结论

1. pi 扩展，目录 `agent/home/extensions/telemetry/`，与 `diagnostics` 平级。零 npm 依赖。普通 HTTPS 使用全局 `fetch`；专用 CA 请求使用 Node `https`，校验链、有效期与主机名。
2. 采集订阅会话事件，内存累计：轮次结束（`turn_end`）把累计状态重写为该会话的草稿；会话结束（shutdown）定稿成一条会话记录。不改现有工具代码。
3. TUI 与 Web 共用同一套采集规则，只有 shutdown 实现不同：TUI 是 pi 的 `session_shutdown` 事件，Web 是运行层销毁实例时调用定稿，见下方「TUI 与 Web 会话」。
4. TUI 与多场 Web 会话并行时各有一份采集实例：各自累计、各写各的草稿；各端点队列与发送经 `globalThis` 单例协调，不依赖扩展实例间共享模块状态。
5. 同一记录写入每个端点的独立队列，分别发送批量信封。确认只删除该端点的记录；失败或停采只作用于该端点。正常退出等待定稿落盘，不等待网络响应。采集和上传失败不影响诊断，不向队员输出。
6. 身份由上传凭据决定：发送时带上队员的 OAuth 访问令牌，接收端解析出 `mid` 与 `deviceId`。采集侧不读凭据、不算指纹。
7. 尽力上传，允许有限丢失，不为此引入可靠消息机制。

## 模块划分

| 文件 | 职责 | I/O |
|---|---|---|
| `index.ts` | 入口：注册事件订阅，串联采集、草稿、定稿、上报；进程内首实例执行恢复 | 无 |
| `collect.ts` | 采集器：事件 → 内存累计状态。每会话实例一份，纯数据结构，可单测 | 无，不做任何同步 I/O |
| `draft.ts` | `agent/home/telemetry/drafts/`：轮次级草稿的重写、定稿删除、启动恢复 | 磁盘 |
| `record.ts` | 定稿时把累计状态序列化为一条记录 | 读 ctx 快照；`kitVersion` 按扩展目录上溯包根读 `VERSION`，不依赖启动目录 |
| `credential.ts` | 被动读 `auth.json` 取上传凭据与有效期 | 磁盘 |
| `outbox.ts` | `agent/home/telemetry/pending-<URL的SHA256>.jsonl`：每端点独立追加、读出、确认移除与限额；按 `recordId` 去重 | 磁盘 |
| `delivery.ts` | 同一记录写入各端点队列，迁移单端点文件，独立发送和确认 | 磁盘与网络 |
| `transport.ts` | 批量 POST、超时、响应分类 | 网络 |
| `config.ts` | `telemetry.json` 与 `currency.json` 读取 | 磁盘 |

## 采集

采集器三条纪律：只改内存（微秒级返回）；不做任何同步 I/O；整体 try/catch，任何一步出错跳过本次累计。草稿写盘不属于采集器：编排层在每个 `turn_end` 先累计、后重写草稿，写失败静默跳过，不影响会话。

| 事件 | 累计内容 |
|---|---|
| `session_start` | `sessionId`、`reason`、`startedAt`、`channel`；进程内首个初始化的实例顺带执行草稿恢复。同实例再收到 `session_start`（上一会话未经 shutdown 被顶替）时，先把旧累计按 `crash` 定稿再开新会话 |
| `input` | `source = interactive` 或 `rpc` 时 `prompts++` |
| `before_provider_request` | 保存本次请求的思考档位；缺失时保留未知，不用结束时的设置反推 |
| `turn_start` / `turn_end` | 配对求间隔累加 `activeMs`；`turns++`；`message.usage` 按 provider + model + thinkingLevel 分组累加进 `models`；`ctx.getContextUsage()` 返回 `undefined` 或 `tokens` 为 `null` 时跳过采样，否则记峰值 `contextPeak` 与同次的 `contextWindow`；`stopReason = aborted` 计 `aborted`；`stopReason = error` 时：本往返无 `after_provider_response` 则 `networkErrors++`，`errorMessage` 原文按文本分组进 `errors`；`turn_end` 后把累计状态重写为该会话草稿并记 `lastTurnEndedAt` |
| `after_provider_response` | 非 2xx 状态码计 `providerErrors`；标记本往返有响应（供 `networkErrors` 判定） |
| `tool_execution_start` / `_end` | `toolCallId` 配对：`tools` 公共维度（`calls`/`failures`/`totalMs`/`maxMs`/`resultBytes`/`truncated`）；扩展字段：`runbook` 取结果的 `runbook.items` 累加 `entriesTotal`、`web_search` 按 provider 计 `providers`、`fetch_content` 按 mode 计 `modes`；`degraded` 按下方结果取值表统计，每次调用最多计一次 |
| `tool_call` | 从入参提取 `scope`；省略时按工具默认值记录（sys 为 overview、driver 为 problem、eventlog 为 recent），无 scope 工具省略 |
| `session_compact` | `compactions++`；`compactionEntry.tokensBefore` 累加进 `compactionTokens`；`reason = overflow` 计 `compactionOverflows` |
| `session_compact_failed` | `compactionFailures++`。只计数，不收 `errorMessage`：报错原文的例外只覆盖 turn 级 |
| `session_shutdown`（TUI）/ 实例销毁（Web） | 定稿：`endedAt`、`endReason`；`contextEntries = buildContextEntries().length`；`turns` 与 `prompts` 均为 0 时不产生记录；序列化、追加 outbox、删草稿、触发上报（不等待） |

并行工具调用会让 start/end 交错，配对靠 `toolCallId`。

约定：

- 契约外工具只采公共维度，新增工具不改契约（见 [../../contract.md](../../contract.md)「工具采集」）。
- 参数值与输出正文不采。
- 报错原文完整入记录，不截断。同文本只留一份并按 `count` 累加，避免重复撑大记录。
- 时间序列化用带本地时区偏移的 ISO 8601（`new Date().toISOString()` 是 UTC，不符合契约）。
- `cost` 与 `currency` 一起写入。映射表可用但缺少 provider 时用缺省 `USD`；文件不可读或不合法时留空，见[计费币种](../../contract.md#计费币种)。模型价目表与币种单位须在实现前核对。
- 管理员探测（Windows PowerShell）耗时秒级，不在事件路径等待：`session_start` 只发起探测，定稿时取结果，草稿期间未完成按非管理员记录。
- `kitVersion` 不依赖启动目录：优先按扩展目录上溯包根读 `VERSION`，回退启动目录；读不到留空且不缓存，下次事件重试。

### 结果字段取值

按工具的实际包装结构读取，同一次调用的同一指标最多计一次：

| 字段 | 取法 | 限制 |
|---|---|---|
| `degraded` | `sys` / `driver` / `eventlog` 读取 `details[scope].degraded === true`；`startup` 读取 `details.startup.degraded === true`；`disk` 读取顶层 `degraded`，或 `usage.method === "node-walk"` / 非空 `usage.degradedFrom` | 仅顶层判断会漏记按 scope 包装的结果；普通 `notice` 不作为降级标记 |
| `truncated` | A 诊断工具：结果文本含 `outputTruncated`。`diagnosticResult` 超限时只改文本，`details` 保持原对象，所以用一次子串扫描判断，不解析 JSON。B 内置 `bash`：`details.truncation.truncated` | `details.truncation` 对诊断工具不存在；`read`/`ls`/`find`/`grep` 的裁剪只写在文本里，没有结构标记，不计数 |
| `entriesTotal` | 取结果的 `runbook.items`，工具已给出命令条数 | 入参字段名是 `items`（元素为 `{ summary, command, shell, admin? }`），没有 `commands` |

### TUI 与 Web 会话

两端共用同一套采集规则：轮次级草稿一致，定稿概念一致，差异只在 shutdown 的实现与会话起止原因的取值。

| 端 | 事件来源 | shutdown 实现 |
|---|---|---|
| TUI | 扩展实例随 TUI 会话加载，`pi.on(...)` 订阅本会话事件 | pi 的 `session_shutdown` 事件 |
| Web | 每场 Web 会话独立加载扩展实例（`WebSessionPool`，mode `rpc`），各自订阅本会话事件 | 运行层销毁实例时调用定稿，销毁点只有两处：删除会话（`deleteSaved`）与 Web 整体关闭（`close`，含进程退出）。切换页面不销毁实例 |

`reason` / `endReason` 的取值按端收敛：

| 取值 | TUI | Web |
|---|---|---|
| `reason: new / resume / fork` | 会话替换 | 新建 / 打开历史（含 TUI 交接）/ 派生（`createBranchedSession`，新会话 ID，源实例继续累计） |
| `reason: startup / reload` | 有 | 无（Web 无 `/reload`，也不随进程启动建会话） |
| `endReason: quit` | 进程退出 | 进程退出（`close` 逐实例销毁） |
| `endReason: new / resume / fork / reload` | 会话替换时旧会话定稿 | 无（Web 切换不销毁实例） |
| `endReason: delete` | 无 | 删除会话（`deleteSaved`），实例中途销毁 |
| `endReason: crash` | 两端同：强杀后恢复补记 | 同左 |

`channel` 字段（`tui｜web`）由加载环境决定，Web 运行层创建会话实例时标记。

实现前核对两项已出结论：SDK 会话（mode `rpc`）的事件覆盖面与 TUI 一致，`session_start`、`input`、`turn_start`/`turn_end`、`tool_call`、`tool_execution_*` 与 provider 事件均正常派发（e2e 已验证）；`AgentSession.dispose()` 不发任何事件（已核对内核源码），运行层显式定稿调用保留。Web 会话的 `session_start` reason 由 `WebSessionPool` 经 `sessionStartEvent` 显式传入（new / resume / fork）。

## 草稿与恢复

草稿是进行中会话的落盘形态：`agent/home/telemetry/drafts/<sessionId>.json`，内容为该会话的累计状态加 `lastTurnEndedAt`（最近一个 `turn_end` 时刻）。按会话分文件，多实例并行写互不干扰。

| 动作 | 规则 |
|---|---|
| 重写 | 每个 `turn_end` 整体重写（先写临时文件再改名），首个轮次时创建；同一会话的写操作经内存串行链排队，避免连续轮次的改名乱序 |
| 定稿 | 补 `endedAt`、`endReason`、`contextEntries` 后写入各端点队列，删草稿。`turns` 与 `prompts` 均为 0 的会话不产生记录，启动即 `/web` 的 TUI 待机会话由此自然跳过 |
| 恢复 | 进程启动时扫 `drafts/`：残留草稿补成 `endReason = crash` 的记录，`endedAt` 取 `lastTurnEndedAt`，`contextEntries` 留空，写入各端点队列后清除；复用草稿内预生成的 `recordId`，接收端去重兑底双份 |

工具包常驻U盘，拔盘、断电、机主电脑崩溃都会强杀进程，草稿把丢失窗口从整场会话缩到一轮。草稿只在首个轮次后存在，尚无完成轮次的会话被强杀时无草稿可恢复，全丢，接受。

## 上报

### 端点

请求体形状见 [信息收集契约](../../contract.md)「上传信封」。

```
POST /v1/sessions
Authorization: Bearer <OA 访问令牌>
```

发送端只负责把令牌放进请求头，鉴权与校验由接收端负责。

### 响应与处理

| 响应 | 处理 |
|---|---|
| 202 | 从对应端点的队列移除该批，其他端点不变 |
| 401 / 403 | 凭据无效、过期，或设备被吊销、改密。保留记录，停止本轮发送，等队员重新登录后由下一次 `session_start` 或草稿恢复补发 |
| 400 / 413 | 这一批本身不合法。丢弃该批，避免坏批反复堵住队列 |
| 429 / 5xx / 网络错误 / 超时 | 保留，等下次。单次请求超时 5 秒 |
| 404 / 410 | 端点不再接受上报，视为停采指令。停止发送，记录保留不删。停采只借状态码传达，接收端响应里不带指令字段，见 [接收端](../receiver/SPEC.md)「接口」 |
| 磁盘只读或空间不足 | 定稿写盘失败时丢弃当前记录；既有队列仍可发送 |
| 功能关闭 | 不采集、不落盘、不联网 |

以上均不提示队员。

### 队列

每个端点的 URL 经 SHA-256 转为文件名，保存在 `agent/home/telemetry/pending-<hash>.jsonl`。本地队列不承诺必达。

1. 同一会话定稿后，使用相同 `recordId` 写入各端点。正常结束实例前等待写盘，不等网络结果。
2. 各端点分别读取和组批，确认后只移除自己的记录。认证失败、超时、停采和坏批均按端点处理。
3. 每端点最多 200 条；追加时去重并淘汰最旧记录，临时文件替换失败时保留原队列。
4. `session_start`、草稿恢复和定稿后触发发送。登录完成不单独触发，后续会话开始或结束时读取新凭据。
5. 单端点 `pending.jsonl` 在启动时先复制到当前全部端点，再清除原记录；中断重试按 `recordId` 去重。
6. 从配置移除端点后不再发送，保留它的本地队列；重新加入同一 URL 可继续补发。

多实例并发的协调全部挂 `globalThis` 单例（jiti `moduleCache: false`，扩展实例间不共享模块状态）：

| 机制 | 规则 |
|---|---|
| 串行队列 | 各队列的文件读写排队执行；网络请求不占用文件队列 |
| 发送单飞 | 每个 home 和端点同时只有一个在途批次；不同端点并发，发送期间的新触发会安排后续读取 |
| 恢复单次 | 进程内首个初始化的实例执行草稿恢复，后续实例跳过 |

会丢记录的情况都已知并接受：超限淘汰；磁盘只读或写入失败；尚无完成轮次的会话在定稿前被强杀；pi 退出时最后一次发送尚未返回；两个 pi 进程共用同一 home 时，队列读改写与另一边追加存在覆盖窗口（串行队列只限进程内）。为消除这些情况而引入确认与重放机制，成本高于收益。

## 身份与凭据

OAuth 已落地；发送端只被动读取 `auth.json` 中的团队 provider 凭据，不刷新、不写回。

记录里不带任何身份字段，队员归属由接收端从上传凭据解析。

| 项 | 取法 |
|---|---|
| 上传凭据 | 被动读 `agent/home/auth.json` 中团队 OAuth provider 条目的 `access` 与 `expires`。只解析文件，不联网、不刷新、不写回 |
| 无凭据或已过期 | 不发送，记录留在队列，队员重新登录后补发 |

不在采集与上报路径上调用会触发刷新的鉴权接口。那类调用会为算一个标识而联网并写盘，把采集变成会影响正常认证的动作。令牌本身也会轮换，任何派生自令牌的标识都不稳定，所以身份一律交给接收端从凭据本身解析。

## 配置

`agent/home/telemetry.json`，随发行写入：

```json
{
  "enabled": true,
  "endpoints": [
    { "url": "https://www.cstoa.top/api/telemetry" },
    { "url": "https://8.163.28.9:8445/api/telemetry", "caFile": "timserver_1.crt" }
  ],
  "authProvider": "cstoa"
}
```

`caFile` 从遥测扩展目录读取，仅信任对应端点。证书缺失或无效时保留队列，不忽略 TLS 验证；公网端点必须为 HTTPS。配置在启动时加载，修改后重启。

独立文件不用 `settings.json`：pi 未向扩展暴露设置管理器，自定义字段易受校验与迁移影响。

计费币种映射表是扩展目录下的 `currency.json`（`agent/home/extensions/telemetry/currency.json`），形状与规则见 [契约](../../contract.md)「计费币种」。它不跟 `telemetry.json` 同目录：`telemetry.json` 在 `agent/home/` 下，由发行脚本生成；`currency.json` 随扩展目录整个复制。

源码常量：`POST_TIMEOUT_MS = 5000`、`BATCH_MAX_RECORDS = 50`、`OUTBOX_MAX_RECORDS = 200`、`ERRORS_MAX_GROUPS = 10`。

## 联网前提

`pi.cmd` 的 `PI_OFFLINE=1` 只关 pi 自身启动联网，不拦扩展请求（已在 0.85.1 的 dist 核对）。`settings.json` 的 `enableInstallTelemetry: false` 关的是 pi 自带遥测，与本扩展无关。

发行配置：`pack/release-settings.mjs` 提供唯一取值，`pack/pack.mjs` 在发行树生成 `agent/home/telemetry.json`。`agent/home/extensions` 整目录复制，扩展代码与 `currency.json` 不用改白名单。
