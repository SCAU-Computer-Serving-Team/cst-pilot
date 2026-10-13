# 接收端

本页定义接收接口、身份校验、数据存储与保留期。契约版本为 0.1。字段见[统计口径](../schema.md)和[信息收集契约](../../contract.md)，部署见[遥测部署](../deployment.md)，验证状态见[遥测验证](../../test/telemetry-report.md)。

## 结论

1. 单个常驻进程加一个 SQLite 文件，只做两件事：收记录、写数据库。看板与告警后置。
2. 一个写端点 `POST /v1/sessions`，一个 `GET /healthz`。不开对外读接口，报表走命令行导 CSV。
3. 一场会话一条记录，原始记录整条存 `payload`，常用字段抽成列。
4. 身份由队员的 OA 登录决定：接收端拿上传的令牌向 OA 内省，取 `mid` 与 `device_id`。
5. 使用 Go 和纯 Go SQLite 驱动，编译为单个可执行文件。依赖仍需进行安全检查。
6. 身份解析使用已部署的 OA 内省接口。接收端服务凭据仅保存于服务器，不进入工具包。

## 接口

请求体形状见 [信息收集契约](../../contract.md)「上传信封」。

```
POST /v1/sessions
Authorization: Bearer <OA 访问令牌>
```

| 响应 | 含义 | 发送端会怎么做 |
|---|---|---|
| 202 `{ accepted, rejected, reasons }` | 收下整批，校验不过的条目在 `reasons` 里计数 | 从队列移除该批 |
| 401 | 令牌无效或过期 | 留住记录，等重新登录 |
| 403 | 设备被吊销或改密 | 停止本轮发送，保留记录 |
| 400 / 413 | 请求本身不合法 | 丢弃该批 |
| 429 / 5xx | 限流或服务端故障 | 留住记录，下次再发 |

约定：

1. 从 `Authorization` 头取令牌，内省通过后把 `mid` 与 `device_id` 写进每条记录。令牌不过的请求不入库。
2. 逐条校验，非法条目计数上报，不拒绝整批。
3. `receivedAt` 与 `ip` 由服务端补充。`ip` 只存网段。
4. 单批上限见契约「上传信封」，超出**整批拒绝，不截断**。截断会让发送端把没送到的记录也一起删掉。
5. 被限流的请求不写上传日志，限制重复请求造成的磁盘写入。
6. 响应不带任何指令字段。停采只借 404 / 410 传达。

## 身份解析

OA 设备授权与内省接口已部署。接收端使用服务凭据调用内省，工具包只上传 Agent 访问令牌。

上传令牌的载荷里有 `mid`、`typ`、`scope`、`device_id`、`pv`、`exp`、`jti`，见 [../../auth/README.md](../../auth/README.md)「令牌与设备标识」。

接收端通过 OA 内省验证令牌。签名密钥仅保存在 OA，不分发给接收端。

```
POST <OA_INTROSPECT_URL>
Authorization: Bearer <服务凭据>
{ "token": "<访问令牌>" }
→
{ "active": true, "mid": "M1024", "device_id": "…", "pv": 3, "exp": 1758000000 }
```

| 内省结果 | 接收端回 | 处理 |
|---|---|---|
| `active: true` | 继续 | 把 `mid` 与 `device_id` 补进每条记录 |
| `reason: expired` / `invalid` | 401 | 不入库，记 `uploads` |
| `reason: revoked` / `password-changed` | 403 | 同上 |
| 超时或 5xx | 503 | 同上，发送端会保留重试 |

**不校验作用域。** 采集范围由团队规定，不作为队员的可选项，见 [../../contract.md](../../contract.md)「身份来源」。

结果按令牌原文的 SHA-256 缓存 60 秒，只存内存，不落库。

## 校验

### 整批拒绝

| 情况 | 状态码 |
|---|---|
| 令牌无效或过期 | 401 |
| 设备被吊销或改密 | 403 |
| 内省不可用 | 503 |
| JSON 解析失败 | 400 |
| `records` 不是数组，或为空 | 400 |
| `records` 条数 > 50 | 400 |
| 请求体 > 1 MB | 413 |

### 逐条拒收

只有三项必备：

| 检查 | 拒收原因 |
|---|---|
| `v` 在 `ACCEPTED_VERSIONS` 里 | `unknown-version` |
| `type === "session"` | `unknown-type` |
| `recordId` 是非空字符串且不长于 64 字符 | `bad-record-id` |

数字字段类型不符时按 0 记，数组缺失按空数组记，`kitVersion` 缺失留空。数组元素数超限时拒收：

| 检查 | reason |
|---|---|
| `tools` ≤ 512 | `too-many-elements` |
| `models` ≤ 32 | `too-many-elements` |
| `errors` ≤ 10 | `too-many-elements` |

202 响应确认整批，发送端不会重发被拒条目。接收端只拒绝协议明确列出的情况。

### 不检查的

| 不查 | 理由 |
|---|---|
| 业务合理性，如 `cost` 为负、`endedAt` 早于 `startedAt` | 上报路径不做数据质量判断，口径问题在统计时处理 |
| 未知字段 | 向前兼容的唯一手段，原样存进 `payload` |
| 记录里自带的身份字段 | 不拒，**覆盖**成内省结果。身份永远来自内省，不信任请求体 |

## 存储

### `sessions`

| 列 | 说明 |
|---|---|
| `record_id` | 主键，去重 |
| `mid`、`device_id`、`received_at`、`ip_net` | 服务端写入 |
| `v`、`kit_version`、`session_id`、`channel` | 记录自带 |
| `reason`、`end_reason`、`started_at`、`ended_at`、`duration_ms`、`active_ms`、`prompts`、`turns`、`context_entries` | 会话 |
| `compactions`、`compaction_tokens`、`compaction_overflows`、`compaction_failures`、`context_peak`、`context_window` | 上下文 |
| `network_errors`、`aborted`、`tool_failures` | 失败 |
| `os_version`、`os_arch`、`admin` | 环境 |
| `cost_cny`、`cost_usd`、`unpriced_turns` | 费用，分币种 |
| `payload` | 整条记录 JSON |

`payload` 保存原始记录，其他列从中派生，同一事务完成。派生规则变化时，可从原始记录重算。

`models[]`、`tools[]`、`errors[]` 和 `providerErrors` 保存在 `payload` 中，使用 JSON 函数查询。

费用必须分币种存：同一天可能既走 `cstoa`（CNY）又走 pi 内置 provider（USD），合成一列等于把两种货币相加。币种查不到的不进任何一列，计入 `unpriced_turns`。

索引：`(received_at, mid)`、`(device_id, received_at)`、`(session_id)`、`(kit_version)`。

### `devices`

| 列 | 说明 |
|---|---|
| `device_id` | 主键 |
| `first_seen_at`、`last_seen_at` | 首末上报 |
| `os_version`、`os_arch`、`admin`、`kit_version` | 最近一次 |
| `last_ip_net` | 最近来源网段 |

每次收到上报时更新。不设吊销标志，吊销由内省决定。

### `members`

| 列 | 说明 |
|---|---|
| `mid` | 主键 |
| `name`、`team` | 编号到姓名、队伍的翻译 |
| `active`、`synced_at` | 在队状态与同步时间 |

只做翻译，不做鉴权，不做外键强制。库里没有的 `mid` 照常入库，报表直接显示编号。

成员资料的导入与同步待验收，见[遥测运维](../../issues.md#遥测运维)。

### `daily_rollup`

| 列 | 来源 |
|---|---|
| `date`、`mid` | 主键。日期按 `received_at` 归到 +08:00 |
| `sessions`、`duration_ms`、`active_ms`、`prompts`、`turns`、`context_entries` | 各列累加 |
| `compactions`、`compaction_overflows`、`compaction_failures` | 各列累加 |
| `context_peak`、`context_window` | 取占比最高的那一条记录的两个值，成对写入 |
| `tool_calls`、`tool_failures`、`degraded` | `payload.tools[]` 累加 |
| `input_tokens`、`output_tokens`、`cache_read_tokens`、`cache_write_tokens`、`total_tokens` | `payload.models[]` 累加 |
| `cost_cny`、`cost_usd`、`unpriced_turns` | 分币种 |
| `aborted`、`network_errors` | 累加 |
| `error_groups`、`error_turns` | 只记组数与出错往返数，**不存原文** |
| `models`、`tools` | JSON，按 provider + model + thinkingLevel、按 name + scope 的分布 |

聚合表只保存错误计数，不保存报错原文，确保明细的 180 天清理有效。

### `uploads`

每个未被限流的上传请求写入一条接收结果。

| 列 | 说明 |
|---|---|
| `id` | 自增主键 |
| `batch_id`、`received_at` | 请求标识与服务端接收时刻 |
| `mid`、`device_id` | 内省结果，鉴权失败时留空 |
| `record_count`、`accepted`、`rejected` | 条数 |
| `inserted` | 真正新增的行数，小于 `accepted` 说明是重发 |
| `rejected_detail` | JSON：`[{ index, recordId, reason }]` |
| `status_code`、`error` | 回给发送端的状态码与非 2xx 时的简短原因 |
| `body_bytes`、`duration_ms`、`ip_net` | 请求侧信息 |

不记请求体原文、令牌与令牌哈希。`reason` 里不放被拒记录的内容。

## 保留期

| 表 | 保留 | 清理动作 |
|---|---|---|
| `sessions` | 永久 | 每天清 180 天前 `payload` 里的 `errors` 字段，其余字段一条不动 |
| `daily_rollup`、`devices`、`members` | 永久 | 无 |
| `uploads` | 90 天 | 每天删过期行 |

报错原文是整条记录里唯一可能回显路径、账号或请求片段的部分，单独设期，与 OA 审计保留期同值。

删除支持按 `device_id` 或 `mid` 筛选，通过命令行执行，不提供 HTTP 删除接口。

## 运维接口

只提供写入与健康检查，不提供 HTTP 读接口。明细导出使用 `export-csv`；汇总重算使用 `rollup`；按成员或设备删除使用 `delete`。

`GET /healthz` 返回 `ok`、会话条数、提交版本 `version` 与构建时间 `builtAt`。服务配置、备份和证书续期见[遥测部署](../deployment.md)，未完成验证集中在[待处理议题](../../issues.md#遥测运维)。
