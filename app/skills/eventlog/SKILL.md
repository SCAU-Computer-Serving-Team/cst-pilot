---
name: eventlog
description: eventlog 事件日志参考。查近期告警（recent）、开关机与蓝屏（boot）、应用崩溃（crash）、服务故障（service）、磁盘错误（disk）、登录审计（security）、自定义查询（query）或单条原文（detail）时查阅。
---

# eventlog

只读 Windows 事件日志工具，负责排查历史故障线索（实时负载归 `sys`）。除 `security` 需管理员权限外，其余 `scope` 普通权限可用。

## 排查路径

1. 未知故障方向先查 `recent`，看 `counts` 高频错误定位来源；明确症状直接选 `boot`、`crash`、`service`、`disk` 或 `security`。
2. 命中关键记录后，取样本中的 `logName` 与 `recordId` 调 `detail` 获取完整原文。
3. 预置场景未覆盖的通道或事件 ID 改用 `query`。
4. 核对 `total`、`truncated`、`scanTruncated`、`unreadable`、`degraded`、`noMatch` 与 `elapsedMs`；排时间线时与 `sys(overview).uptime.bootTime` 对照（均为本机时区）。

## 参数

| 参数 | 适用 `scope` | 约束与默认值 |
|---|---|---|
| `scope` | 全部 | `recent`（默认）、`boot`、`crash`、`service`、`disk`、`security`、`query`、`detail` |
| `hours` / `top` | 除 `detail` 外 | 时间窗小时数（默认 `24`，上限 `720`）/ 样本条数（默认与上限均为 `100`） |
| `level` | `recent`、`query` | `warn`（Critical/Error/Warning，`recent` 默认）或 `error`（Critical/Error）。`query` 省略时不限级别 |
| `kind` | `boot` | `all`（默认）、`unexpected`、`bluescreen` |
| `type` | `security` | `all`（默认）、`logonFail`、`lockout` |
| `app` / `name` / `msg` | `crash` / `service` / `query` | 消息字面子串过滤（1–200 字符，忽略大小写） |
| `logName` | `query`、`detail` | 通道名（`query` 默认 `"System"` 单通道；`detail` 必填） |
| `ids` / `provider` | `query` | 最多 16 个 `0–65535` 事件 ID / 提供程序正则（1–128 字节，忽略大小写） |
| `recordId` / `id` | `detail` | 二选一必填，须同时传 `logName`。`recordId` 精确匹配，`id` 取该 ID 最新一条 |

## 各 scope 覆盖范围

| `scope` | 通道与内置范围 |
|---|---|
| `recent` | System + Application 通道的 warn/error 汇总 |
| `boot` | System 通道。`all`：启停 12/13/6005/6006/6009、意外重启 41/6008、蓝屏 1001、重启原因 1074/19/7045 及 `Microsoft-Windows-WHEA-Logger` 全部事件；`unexpected`：41/6008；`bluescreen`：1001 + `WHEA-Logger`。ID 19 可能来自更新或硬件错误，按 `provider` 区分 |
| `crash` | Application 通道。`Application Error`/1000 与 `.NET Runtime`/1026（限 Error），加不限级别的 `Windows Error Reporting`/1001、`Application Hang`/1002、`SideBySide`/33·35 |
| `service` | System 通道 Service Control Manager 的 18 个故障 ID（7000–7003、7009、7011、7013、7022–7026、7031、7032、7034、7038、7041、7043）。新服务安装 7045 归 `boot` |
| `disk` | System 通道 7/11/51（坏块、控制器、分页错误）、129/153（IO 超时重试）、50/55/98/140（写失败、NTFS 损坏）、157（掉盘） |
| `security` | Security 通道 4624（登录成功）、4625（`logonFail`）、4740（`lockout`）。仅覆盖本地与工作组，不含域 Kerberos 4771。非管理员不执行查询，返回 `admin: false` 与 `degraded: true` |
| `query` | 白名单外自定义查询，默认仅查 `System` 通道；查应用日志须传 `logName: "Application"` |
| `detail` | 返回单条完整原文（保留换行，上限 20,000 字符） |

## 返回字段与判读

返回结构为 `{ "<scope>": { ... }, "elapsedMs": N }`。

1. **列表类字段**：`logs`、`hours`、`top`、`total`、`truncated`、`events[]`、`counts[]`、`countsTruncated`、`unreadable`、`firstTime`、`lastTime`、`admin`、`notice`，可选 `noMatch: true`、`scanTruncated: true`、`degraded: true`、`collectionErrors`。
2. **`events[]` 与时区**：每条含 `logName`、`time`（本机带偏移 RFC3339，如 `2026-11-26T00:35:00+08:00`）、`recordId`、`level`、`levelName`（固定英文映射）、`provider`、`id`、`msg`（上限 200 字符，无模板时为 `null`，正文随系统语言本地化）。
3. **`counts[]`**：每项含 `key`（`provider/id`）、`provider`、`id`、`n`、`last`，按 `n` 降序最多 100 组，超出置 `countsTruncated: true`。
4. **`detail` 字段**：`found`、`logName`、命中时附完整事件字段与 `machine`；正文超 20,000 字符置 `msgTruncated: true` 与 `originalChars`；消息不可读时 `msg` 为 `null` 且 `degraded: true`。
5. **完整性与错误**：
   - `noMatch: true` 仅在无错误、无不可读、未截断且 `total === 0` 时出现，可据此确认该范围内无命中记录。
   - 单通道触及 100,000 条扫描上限时置 `scanTruncated: true` 与 `degraded: true`，须缩小 `hours` 重查。
   - `unreadable > 0` 会置 `degraded: true`，其余记录仍可用。
   - 请求通道全部失败返回 `isError: true`；部分通道失败或 `security` 缺管理员权限置 `degraded: true`，禁止将空数组解释为无异常。
   - 超时上限 30 秒；`elapsedMs` 偏高说明日志量过大或查询过滤过宽，可缩小 `hours` 或补充 `ids`。
