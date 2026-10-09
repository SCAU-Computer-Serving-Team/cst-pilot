# 接收端

状态：Go 接收端已部署，真实 OA 内省已配置；本机发送端与接收端联调通过，真实队员公网上传和备份恢复待验收。契约版本 0.1。更新：2026-10-08。会话记录字段见 [schema.md](../schema.md)，共用字段、身份来源与计费币种见 [../../contract.md](../../contract.md)，系统切块见 [../architecture.md](../architecture.md)。

## 结论

1. 单个常驻进程加一个 SQLite 文件，只做两件事：收记录、写数据库。看板与告警后置。
2. 一个写端点 `POST /v1/sessions`，一个 `GET /healthz`。不开对外读接口，报表走命令行导 CSV。
3. 一场会话一条记录，原始记录整条存 `payload`，常用字段抽成列。
4. 身份由队员的 OA 登录决定：接收端拿上传的令牌向 OA 内省，取 `mid` 与 `device_id`。
5. 除标准库外只依赖纯 Go 的 SQLite 驱动：免运行时安装、免漏洞扫描、交叉编译单二进制部署。
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
| 403 | 设备被吊销或改密 | 停止发送，不删数据 |
| 400 / 413 | 请求本身不合法 | 丢弃该批 |
| 429 / 5xx | 限流或服务端故障 | 留住记录，下次再发 |

约定：

1. 从 `Authorization` 头取令牌，内省通过后把 `mid` 与 `device_id` 写进每条记录。令牌不过的请求不入库。
2. 逐条校验，非法条目计数上报，不拒绝整批。
3. `receivedAt` 与 `ip` 由服务端补充。`ip` 只存网段。
4. 单批上限见契约「上传信封」，超出**整批拒绝，不截断**。截断会让发送端把没送到的记录也一起删掉。
5. 被限流的请求不写日志，否则限流本身会成为写入放大器。
6. 响应不带任何指令字段。停采只借 404 / 410 传达。

## 身份解析

OA 设备授权与内省接口已部署。接收端使用服务凭据调用内省，工具包只上传 Agent 访问令牌。

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
| `v`、`kit_version`、`session_id`、`channel` | 记录自带 |
| `reason`、`end_reason`、`started_at`、`ended_at`、`duration_ms`、`active_ms`、`prompts`、`turns`、`context_entries` | 会话 |
| `compactions`、`compaction_tokens`、`compaction_overflows`、`compaction_failures`、`context_peak`、`context_window` | 上下文 |
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
| `sessions`、`duration_ms`、`active_ms`、`prompts`、`turns`、`context_entries` | 各列累加 |
| `compactions`、`compaction_overflows`、`compaction_failures` | 各列累加 |
| `context_peak`、`context_window` | 取占比最高的那一条记录的两个值，成对写入 |
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
| 运行时 | Go，编译单二进制（约 11MB） | 目标机是 2G 内存 Linux：常驻约 12MB，无需运行时 |
| 语言 | Go，交叉编译 `CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -o telemetry-receiver ./src` | 无解释器，部署只拷一个文件 |
| HTTP | `net/http` | 两个路由，不引框架 |
| 数据库 | modernc.org/sqlite（纯 Go，无 cgo） | WAL；页缓存上限 2MB、不开 mmap |
| 令牌内省 | `net/http` 客户端 | 60 秒缓存，SHA-256 键 |
| 内存上限 | `debug.SetMemoryLimit(24MiB)` | GC 软上限，超限加速回收 |
| 测试 | 主仓库 e2e 拉起二进制做 HTTP 集成 | node:test 验证内省协议、入库计数、去重与发送端确认 |
| 进程 | systemd | 单进程常驻 |
| 定时任务 | 服务内置启动执行和 24 小时定时执行 | 清理保留期与重算汇总；失败写日志 |
| HTTPS | nginx 反代 | 服务只监听 `127.0.0.1` |
| 日志 | 标准输出与错误输出到 journald | |

子命令：`serve`（默认）、`export-csv`、`rollup`、`delete`。members 导入、备份与恢复验证未完成。

代码在独立仓库 `cst-pilot-server` 的 `src/`，不随工具包发行，部署到服务器上单独运行；契约文档随主仓库。五个源文件加 go.mod：

```
cst-pilot-server/
  go.mod         唯一外部依赖 modernc.org/sqlite
  src/
    main.go        子命令分发与环境变量、GC 软上限
    server.go      HTTP：路由、体积限制、整批/逐条分流、uploads 记录
    auth.go        内省（桩/真切换）与 60 秒缓存
    validate.go    整批拒绝与逐条拒收
    store.go       SQLite 建表、写入、去重、devices/uploads、rollup、保留期、CSV 导出
```

配置用环境变量：

```
TELEMETRY_PORT=8787
TELEMETRY_HOST=127.0.0.1
TELEMETRY_DB=/var/lib/cst-telemetry/telemetry.db
OA_INTROSPECT_URL=http://127.0.0.1:8080/api/oauth/introspect   # 生产必须配置真实内省
OA_SERVICE_TOKEN=<接收端服务凭据>
```

## OA 内省接口

线上 OA 使用 `CSTOA_OA_INTROSPECT_SERVICE_TOKEN` 校验服务调用方，接收端使用 `OA_SERVICE_TOKEN` 发送该凭据。成功响应提供 `active`、`mid`、`device_id` 与 `scope`，不返回姓名。

| 项 | 当前行为 |
|---|---|
| 服务未配置凭据 | OA 返回 503 |
| 服务凭据缺失或不匹配 | OA 返回 401 |
| Agent 令牌签名、类型或有效期无效 | `active=false, reason=invalid` |
| 设备、成员或密码版本校验失败 | `active=false, reason=revoked` |
| 正常令牌 | `active=true`，接收端将学号与设备标识写入记录 |

服务内省超时为 5 秒。接收端不向工具包暴露服务凭据。

访问令牌的有效期（默认 2 小时）与「记住 7 天」只影响发送端的补发时机，不阻塞接收端。

## 部署

运行于 cstoa 服务器（8.138.170.239，Alibaba Cloud Linux 3），与 OA、nginx 共存。2026-10-08 已只读核查运行状态：

| 项 | 实际值 |
|---|---|
| 二进制与 env | `/opt/cst-pilot-server/telemetry-receiver`、同目录 `telemetry.env`（root:csttele） |
| 数据库 | `/var/lib/cst-telemetry/telemetry.db`，属主系统用户 `csttele`（无 shell） |
| 服务 | systemd `cst-telemetry.service`，`ProtectSystem=strict` + `ReadWritePaths=/var/lib/cst-telemetry`，开机自启 |
| 内部监听 | `127.0.0.1:8787`，路径 `POST /v1/sessions`、`GET /healthz` |
| 公网入口 | nginx：`location = /api/telemetry` → `127.0.0.1:8787/v1/sessions` |
| 上报地址 | `https://www.cstoa.top/api/telemetry`。裸域 cstoa.top 301 到 www，POST 跟随重定向会丢包，endpoint 必须带 www |
| 身份 | 真实内省，地址 `http://127.0.0.1:8080/api/oauth/introspect`；OA 与接收端服务凭据一致 |
| 内存 | 常驻约 11MB |

当前验证范围见[接入与遥测验证](../../test/telemetry-report.md)。真实队员扫码授权后的公网上传仍待验收。交互 shell 运行导出或删除时，须显式设置 `TELEMETRY_DB`；systemd 环境变量不自动进入 shell。

接收端在启动时执行一次保留期清理与汇总，此后每 24 小时执行。生产汇总表已有数据；清理效果、错误告警与长期运行仍需运维验证。

遥测 SQLite 库使用独立的每日一致性备份，`cst-telemetry-backup.timer` 已启用，CSTOA 使用 Python 3.11。备份保存在 `/var/lib/cst-telemetry/backups`，保留 30 天。异机保存和完整恢复验证待完成。

`GET /healthz` 返回 `ok`、会话条数、提交版本 `version` 与构建时间 `builtAt`。磁盘容量、定时任务和备份成功状态需额外监控，日志走 journald。

### Tim 接收端

`timserver_1` 已部署同款 Go 二进制，独立 Docker Compose 项目 `cst-pilot-telemetry`。源码与部署文件位于独立仓库 `cst-pilot-server/deploy/timserver_1/`。

| 项 | 实际值 |
|---|---|
| 服务器 | `8.163.28.9` |
| 部署目录 | `/srv/cst-pilot-server` |
| 公网上传 | `https://8.163.28.9:8445/api/telemetry` |
| 内部监听 | Docker 网络 `receiver:8787`，不映射宿主端口 |
| 数据库 | `/srv/cst-pilot-server/data/telemetry.db` |
| 身份 | 真实 OA HTTPS 内省；服务凭据仅保存在服务器 |
| HTTPS | 独立 Caddy 网关；专用公开 CA 随扩展发行，CA 私钥不外发 |
| 证书续期 | `cst-telemetry-tls.timer` 每日检查，证书剩余不足 30 天时续签并重启本项目网关 |
| 备份 | `cst-telemetry-backup.timer` 每日在线一致性备份，保留 30 天 |
| 安全组 | 仅新增 TCP 8445 入站规则，不改现有服务端口 |

两端独立存储，不互相复制数据库。客户端发送相同记录，各端分别确认、去重。两端已部署 `ecc1526de415`，二进制 SHA-256 一致；更新前已有会话、凭据与 Tim CA 均保留。两端每日同机备份已启用并执行成功；异机备份、完整恢复和真实队员双端入库仍待验收。

## 报表

第一版只做命令行导出 CSV：当前导出 `sessions` 明细（`export-csv [out]`），读 `daily_rollup` 的按日汇总导出未做，归「议题」。

不做 HTTP 读接口。本地仪表盘只看不上传的请求记录，不读接收端；服务端加读接口要多一套鉴权，换不来对应的收益。

## 议题

| 编号 | 议题 | 状态 |
|---|---|---|
| R13 | 运维归属，谁负责部署、备份与恢复 | 待定 |

其余 R 议题已有结论，写进本文正文。生产验收与备份安排见「部署」。
