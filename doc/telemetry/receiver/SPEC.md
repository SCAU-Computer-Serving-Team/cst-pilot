# 接收端

状态：设计稿，契约版本 0.1。更新：2026-09-20。会话记录字段见 [schema.md](../schema.md)，共用字段、身份来源与计费币种见 [../../contract.md](../../contract.md)，系统切块见 [../architecture.md](../architecture.md)。

## 结论

1. 单个 Node 常驻进程加一个 SQLite 文件，只做两件事：收记录、写数据库。看板与告警后置。
2. 一个写端点 `POST /v1/sessions`，一个 `GET /healthz`。不开对外读接口，报表走命令行导 CSV。
3. 一场会话一条记录，原始记录整条存 `payload`，常用字段抽成列。
4. 身份由队员的 OA 登录决定：接收端拿上传的令牌向 OA 内省，取 `mid` 与 `device_id`。
5. 零 npm 依赖，只用 Node 内置模块。
6. **身份解析依赖 OA 的内省接口，该接口尚未实现**，见「依赖 OAuth 实现」。OAuth 相关的内容一律等它落地后再讨论。

## 接口

```
POST /v1/sessions
Authorization: Bearer <OA 访问令牌>
{ "v": "0.1", "batchId": "<uuid>", "sentAt": "...", "records": [ ... ] }
```

| 响应 | 含义 | 发送端会怎么做 |
|---|---|---|
| 202 `{ accepted, rejected, reasons }` | 收下整批，校验不过的条目在 `reasons` 里计数 | 从队列移除该批 |
| 401 | 令牌无效或过期 | 留住记录，等重新登录 |
| 403 | 设备被吊销或改密 | 停止发送，不删数据 |
| 400 / 413 | 请求本身不合法 | 丢弃该批 |
| 429 / 5xx | 限流或服务端故障 | 留住记录，下次再发 |

约定：

1. 从 `Authorization` 头取令牌，内省通过后把 `mid` 与 `device_id` 写进每条记录。令牌不过的请求不入库。
2. 逐条校验，非法条目计数上报，不拒绝整批。
3. `receivedAt` 与 `ip` 由服务端补充。`ip` 只存网段。
4. 单批上限 50 条或 1 MB，超出**整批拒绝，不截断**。截断会让发送端把没送到的记录也一起删掉。
5. 被限流的请求不写日志，否则限流本身会成为写入放大器。
6. 响应不带任何指令字段。停采只借 404 / 410 传达。

## 身份解析

**待 OAuth 落地。** 本节等 OAuth 实现完成后再定稿，以下为暂定。

上传令牌的载荷里有 `mid`、`typ`、`scope`、`device_id`、`pv`、`exp`、`jti`，见 [../../auth/README.md](../../auth/README.md)「令牌与设备标识」。

**接收端不自己验签。** 令牌是 HMAC 签名，验签需要 OA 的对称密钥，而那把密钥同时用来签发模型访问令牌，外发等于允许伪造任意队员的令牌。接收端把令牌交给 OA 做内省：

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

| 检查 | 不过时 reason |
|---|---|
| `v` 在 `ACCEPTED_VERSIONS` 里 | `unknown-version` |
| `type === "session"` | `unknown-type` |
| `recordId` 是非空字符串且不长于 64 字符 | `bad-record-id` |

其余一律宽容：数字字段类型不对按 0 记，数组缺失按空数组记，`kitVersion` 缺失留空。只有数组元素数超限才拒：

| 检查 | reason |
|---|---|
| `tools` ≤ 512 | `too-many-elements` |
| `models` ≤ 32 | `too-many-elements` |
| `errors` ≤ 10 | `too-many-elements` |

**拒一条就是永久丢一条**：发送端收到 202 会移除整批，不会重发被拒的条目。所以拒收条件取最保守的一组。

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
| `v`、`kit_version`、`session_id` | 记录自带 |
| `reason`、`end_reason`、`started_at`、`ended_at`、`duration_ms`、`active_ms`、`prompts`、`turns`、`messages` | 会话 |
| `compactions`、`compaction_tokens`、`context_peak` | 上下文 |
| `network_errors`、`aborted`、`tool_failures` | 失败 |
| `os_version`、`os_arch`、`admin` | 环境 |
| `cost_cny`、`cost_usd`、`unpriced_turns` | 费用，分币种 |
| `payload` | 整条记录 JSON |

列是派生的，`payload` 是权威的：先写 `payload`，再从它抽列，同一事务完成。抽列的代码改了可以重跑修好历史数据，`payload` 不用动。

`models[]`、`tools[]`、`errors[]`、`providerErrors` 不进列，留在 `payload` 里用 JSON 函数查。报表是每天跑一次的批处理，不值得为它加结构。

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

第一版用命令行导入 CSV，OA 出导出接口后改成定时拉。

### `daily_rollup`

| 列 | 来源 |
|---|---|
| `date`、`mid` | 主键。日期按 `received_at` 归到 +08:00 |
| `sessions`、`duration_ms`、`active_ms`、`prompts`、`turns`、`messages` | 各列累加 |
| `tool_calls`、`tool_failures`、`degraded` | `payload.tools[]` 累加 |
| `input_tokens`、`output_tokens`、`cache_read_tokens`、`cache_write_tokens`、`total_tokens` | `payload.models[]` 累加 |
| `cost_cny`、`cost_usd`、`unpriced_turns` | 分币种 |
| `aborted`、`network_errors` | 累加 |
| `error_groups`、`error_turns` | 只记组数与出错往返数，**不存原文** |
| `models`、`tools` | JSON，按 provider + model + thinkingLevel、按 name + scope 的分布 |

聚合表不存报错原文。存了的话，明细表的 180 天清理就形同不存在。

### `uploads`

每收到一个请求写一行，进出两个方向都记。

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

删除支持按 `device_id` 或 `mid`，走命令行，不做 HTTP 接口。删除是低频运维动作，为此再造一套管理员鉴权不值得。

## 技术栈

| 层 | 选型 | 说明 |
|---|---|---|
| 运行时 | Node.js 24 LTS | |
| 语言 | TypeScript，用 `node` 直接运行 | 类型剥离，无构建步骤；`tsc --noEmit` 只做检查，不进产物 |
| HTTP | `node:http` | 两个路由，不引框架 |
| 数据库 | `node:sqlite`，WAL 模式 | |
| 令牌内省 | 全局 `fetch` | |
| 测试 | `node:test` | 单测校验、抽列、CSV 转义、去重；集成测试用内存库加桩内省 |
| 进程 | systemd | 单进程常驻 |
| 定时任务 | systemd timer | 任务在独立进程跑，崩了不拖累服务 |
| HTTPS | Caddy 反代 | 自动申请与续期证书，Node 只监听 `127.0.0.1` |
| 日志 | `console` 到 journald | |
| npm 依赖 | 0 个 | 免 install、免漏洞扫描、免升级 |

子命令：`serve`（默认）、`rollup`、`cleanup`、`report`、`members`、`delete`。

代码放在本仓库的 `src/telemetry/`，与[发送端](../../sender/SPEC.md)同仓库，但不进工具包的发行包，部署到服务器上单独运行。

```
src/telemetry/
  main.ts        子命令分发
  server.ts      HTTP：路由、体积限制、限流
  auth.ts        内省调用与 60 秒缓存
  validate.ts    逐条校验
  store.ts       SQLite 写入、去重、查询
  columns.ts     从 payload 抽列
  schema.sql     建表
  rollup.ts      每日聚合
  cleanup.ts     保留期清理与备份
  report.ts      CSV 导出
  members.ts     成员 CSV 导入
  delete.ts      按设备或队员删除
  config.ts      环境变量
  deploy/        systemd unit 与 Caddyfile
```

配置用环境变量：

```
PORT=8080
DB_PATH=/var/lib/cst-telemetry/telemetry.db
OA_INTROSPECT_URL=https://cstoa.top/api/oauth/introspect
OA_INTROSPECT_TOKEN=<接收端服务凭据>
ACCEPTED_VERSIONS=0.1
```

## 依赖 OAuth 实现

以下各项要等 OA 的内省接口就绪才能定稿。开发期用桩顶替，接口形状先按本节定死。

| # | 依赖项 | 阻塞什么 | 现在怎么做 |
|---|---|---|---|
| D1 | OA 提供 `POST /api/oauth/introspect` | 身份解析整步 | `auth.ts` 的 `introspect(token)` 返回固定测试身份 |
| D2 | 内省响应区分 `expired`、`invalid`、`revoked`、`password-changed` | 401 与 403 的分法 | 按本文件的四个 reason 约定，等 OA 对齐 |
| D3 | 内省端点的调用方鉴权方式 | `OA_INTROSPECT_TOKEN` 这一项配置 | 先读环境变量，端点定了再改配置项 |
| D4 | `mid` 与 `device_id` 的最终字段名与格式 | `sessions` 表的列定义 | 照契约用 `mid` 与 `deviceId` |
| D5 | 内省的超时与失败语义 | 接收端回 503 还是别的 5xx | 暂定 503 |

访问令牌的有效期（默认 2 小时）与「记住 7 天」只影响发送端的补发时机，不阻塞接收端。

## 部署

1. `cst-telemetry.service` 常驻 `serve`，监听 `127.0.0.1:8080`
2. `cst-telemetry-rollup.timer` 每天 03:00 聚合前一天
3. `cst-telemetry-cleanup.timer` 每天 04:00 清理保留期，并做一次 `VACUUM INTO` 备份，保留 30 天
4. Caddy 收 HTTPS，反代到本机 8080

备份落在同一块盘上只防误删，不防机器丢失。跨机备份列入待办。

监控靠 `GET /healthz`：返回进程、数据库、磁盘与最近入库时间，交给外部探测。日志走 journald。

## 报表

第一版只做命令行导出 CSV：读 `daily_rollup`，一行一个「日期加队员」。

```
node src/telemetry/main.ts report --from 2026-09-01 --to 2026-09-30
```

不做 HTTP 读接口。本地仪表盘只看不上传的请求记录，不读接收端；服务端加读接口要多一套鉴权，换不来对应的收益。

## 议题

| 编号 | 议题 | 状态 |
|---|---|---|
| R1 | 具体部署机器与域名 | 待定 |
| R13 | 运维归属，谁负责部署、备份与恢复 | 待定 |

其余 R 议题已有结论，写进本文正文。等 OA 内省接口的项见「依赖 OAuth 实现」。
