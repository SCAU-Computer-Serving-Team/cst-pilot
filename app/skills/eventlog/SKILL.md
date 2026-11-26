---
name: eventlog
description: eventlog 事件日志查询：近期告警、开关机与蓝屏、应用崩溃、服务故障、磁盘事件、登录审计、自定义查询与单条原文。
---

# eventlog

只读 Windows 事件日志工具，排查历史记录（实时负载归 `sys`）。返回结构为 `{ "<scope>": { ... }, "elapsedMs": N }`。

## 场景选择

| 现象 | 调用 | 结束条件 |
|---|---|---|
| 故障方向不明 | `eventlog({ scope: "recent" })` | 找到异常事件或确认无命中，标明是否截断 |
| 意外重启、蓝屏、崩溃、服务异常、磁盘错误、登录审计 | 选对应预设 `scope`（`boot`、`crash`、`service`、`disk`、`security`） | 列出关键事件与频次，对照 `provider` 与 `msg` 核验原因 |
| 需查看单条日志完整原文 | 取列表中的 `logName` 与 `recordId`，调 `eventlog({ scope: "detail", logName, recordId })` | 读到完整正文，或标明 `msgTruncated` / `outputTruncated` |
| 预设场景未覆盖通道或需加 `ids` 过滤 | `eventlog({ scope: "query", logName, ids?, provider?, level?, msg? })` | 拿到目标记录或明确无命中与权限限制 |

## 参数

| 参数 | 适用 `scope` | 约束与默认值 |
|---|---|---|
| `scope` | 全部 | `recent`（默认）、`boot`、`crash`、`service`、`disk`、`security`、`query`、`detail` |
| `hours` / `top` | 除 `detail` 外 | 正整数，至少为 `1`。`hours` 默认 `24`、上限 `720`；`top` 默认与上限均为 `100` |
| `level` | `recent`、`query` | `warn`（Critical/Error/Warning，`recent` 默认）或 `error`（Critical/Error）。`query` 省略时不限级别；其他 `scope` 传入非法值也会触发校验错误 |
| `kind` | `boot` | `all`（默认）、`unexpected`、`bluescreen` |
| `type` | `security` | `all`（默认）、`logonFail`、`lockout` |
| `app` / `name` / `msg` | `crash` / `service` / `query` | 消息字面子串（1–200 字符，忽略大小写） |
| `provider` | 全部列表类 `scope` | 提供程序正则（1–128 字节，忽略大小写），校验正则语法与复杂度 |
| `logName` | `query`、`detail` | 通道名（`query` 默认 `"System"` 单通道；`detail` 必填）。非空且不超过 100 字节，首字符为 ASCII 字母或数字，其余仅允许 ASCII 字母、数字、空格及 `./_-`；任意 `scope` 传入均会先校验 |
| `ids` | 仅 `query` | 最多 16 个 `0–65535` 整数，空数组不限定 ID |
| `recordId` / `id` | `detail` | 二选一必填，须配 `logName`。`recordId` 为非负整数精确匹配，`id` 取该 ID 最新一条 |

## 各 scope 覆盖范围

| `scope` | 通道与内置条件 |
|---|---|
| `recent` | `System` 与 `Application` 通道的 `warn` 或 `error` 事件 |
| `boot`（`kind: "all"`） | `System` 通道事件 ID 12、13、6005、6006、6009、41、6008、1001、1074、19、7045，或提供程序 `Microsoft-Windows-WHEA-Logger` 全部事件。该条件按 ID 过滤，不限制所有 ID 的提供程序，须结合 `provider` 与 `msg` 判读（如 7045 为新服务安装，19 可能来自更新或硬件错误） |
| `boot`（`kind: "unexpected"`） | `System` 通道事件 ID 41、6008 |
| `boot`（`kind: "bluescreen"`） | `System` 通道事件 ID 1001 或 `Microsoft-Windows-WHEA-Logger` 全部事件 |
| `crash` | `Application` 通道：`Application Error` 1000 与 `.NET Runtime` 1026（限 Error），加不限级别的 `Windows Error Reporting` 1001、`Application Hang` 1002、`SideBySide` 的 33 和 35 号事件 |
| `service` | `System` 通道 `Service Control Manager` 的 18 个故障 ID（7000–7003、7009、7011、7013、7022–7026、7031、7032、7034、7038、7041、7043） |
| `disk` | `System` 通道事件 ID 7、11、50、51、55、98、129、140、153、157 |
| `security` | `Security` 通道事件 ID 4624（登录成功）、4625（`logonFail`）、4740（`lockout`）。需管理员权限；非管理员不查日志，返回 `admin: false`、`degraded: true`（不含 `collectionErrors`） |
| `query` | 自选通道与过滤条件（可查预设 ID），受目标通道实际权限约束 |
| `detail` | 按 `logName` 与 `recordId` 或 `id` 读单条原文（保留换行，上限 20,000 字符） |

## 返回字段与截断判读

1. **列表类字段**：`logs`、`hours`、`top`、`total`（本次扫描命中数）、`truncated`、`events[]`、`counts[]`、`countsTruncated`、`unreadable`、`firstTime`、`lastTime`、`admin`（探测失败时可为 `null`）、`notice`，可选 `noMatch: true`、`scanTruncated: true`、`degraded: true`、`collectionErrors`。
2. **`events[]` 与时区**：每条含 `logName`、`time`（本机带偏移 RFC3339，与 `sys(overview).uptime.bootTime` 同为本机时区）、`recordId`、`level`、`levelName`（固定英文）、`provider`、`id`、`msg`（上限 200 字符，未读到消息时为 `null`）。
3. **`counts[]` 统计口径**：扫描时先收录遇到的至多 100 个不同 `(provider, id)` 组，再将已收录的组按 `n` 降序排列。`countsTruncated: true` 时不保证已收录全量最频繁的 100 组，须缩小 `hours` 或补充条件重查。
4. **`detail` 字段**：`found`、`logName`，命中时附事件字段与 `machine`。超 20,000 字符置 `msgTruncated: true` 与 `originalChars`；`msg` 未读到时为 `null` 且 `degraded: true`。出现 `msgTruncated` 或顶层 `outputTruncated` 时，不可声称已核验完整原文。
5. **完整性与超时**：
   - `noMatch: true` 仅在无错误、无不可读、未截断且 `total === 0` 时出现。
   - 单通道触及 100,000 条扫描上限置 `scanTruncated: true` 与 `degraded: true`；`unreadable > 0` 也会置 `degraded: true`。
   - 请求通道全部失败或超 30 秒返回 `isError: true`（`{"error", "elapsedMs"}`）；部分失败置 `degraded: true`。超限时缩小 `hours`，或切到 `query` 显式指定通道与 `ids` 重查。
