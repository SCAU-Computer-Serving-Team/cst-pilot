---
name: eventlog
description: eventlog 工具的参数与返回字段说明。八个 scope 的查询范围与字段定义、counts 折叠计数表、ID 白名单边界、security 管理员要求、与 sys 的实时/历史分工。
---

# eventlog 工具说明

只读事件日志工具，读取 Windows 记录的历史故障线索，按 `scope` 选择子功能，不传默认 `recent`。除 `security`（登录审计，需管理员）外各 scope 免管理员。与 sys 的分工：sys 看实时负载，eventlog 看历史痕迹。

## 参数

1. `scope`（可选）：`recent` / `boot` / `crash` / `service` / `disk` / `security` / `query` / `detail`，不传默认 `recent`
2. `hours`（可选）：时间窗（小时），默认 24，上限 720；`top`（可选）：事件列表条数上限，默认 100（硬上限 100）；两者除 detail 外通用
3. 分支参数：`level`（recent 默认 warn，包含 Warning 及更严重；query 省略时不限级别）/ `kind`（boot）/ `type`（security）/ `app`（crash）/ `name`（service）/ `ids`+`provider`+`msg`+`logName`（query）/ `recordId` 或 `id` + `logName`（detail，二选一必填）——每支自含全部参数，`logName` 不是全局覆盖参数

## 查询类 scope 的公共返回字段

1. `logs`（实际查询通道）/ `hours` / `top` / `total`（时间窗内命中总数，含未显示的更早记录）/ `truncated`（total > top 时 true）/ `firstTime` / `lastTime`（事件样本的最早 / 最新时间，空列表时无此字段）/ `unreadable`（消息资源损坏被跳过的记录数）/ `noMatch`（条件级零命中提示码）/ `admin`（查询进程是否管理员）/ `events`（最新 top 条，时间倒序）/ `counts`（来源/ID 折叠计数表）/ `notice`
2. `events` 每条：`logName` / `time`（本地时区）/ `recordId`（detail 直取用）/ `level` 数字与 `levelName`（英文固定映射）/ `provider` / `id` / `msg`（≤200 字符简述，无渲染模板的系统事件为 null）
3. `counts` 每项：`key`（provider/id）/ `n`（出现次数）/ `last`（最近一次时间），n 降序最多 100 组，超出标记 `countsTruncated=true`；仅在 `countsTruncated=false` 时要求 `sum(counts.n) === total`
4. `noMatch` 固定码：`provider-not-found` = 提供程序名在本机不存在，`provider-log-mismatch` = 提供程序不写往所查通道

## 各 scope 的查询范围

1. `recent`：System + Application 通道的 warn/error 汇总，开场查询
2. `boot`：`kind=all`（默认）查官方重启排查清单（启停标记 12/13/6005/6006/6009、意外重启 41/6008、蓝屏 1001、重启原因 1074/19/7045）∪ WHEA-Logger 硬件错误；`kind=unexpected` 只查 41/6008；`kind=bluescreen` 查 1001 + WHEA-Logger。ID 19 可能来自 WindowsUpdateClient 或 WHEA-Logger，以 `provider` 字段区分
3. `crash`：Application 通道按提供程序与 ID 配对：Application Error 1000 / .NET Runtime 1026 限 Error；Windows Error Reporting 1001 / Application Hang 1002 / SideBySide 33·35 不限级别，避免漏掉 Information 级 WER 报告。`app` 对全部分组按消息子串过滤。
4. `service`：System 通道 SCM 白名单 18 个 Error 级 ID（启动失败 / 挂起超时 / 意外终止 / 依赖失败等）；7045（新服务安装，Information 级）不在本通道，归 `boot` 白名单；`name` 按服务名子串过滤
5. `disk`：System 通道 7/11/51（坏块/控制器/分页错误）、129/153（IO 超时重试）、55/98/50/140（NTFS 损坏/写失败）、157（掉盘）
6. `security`：Security 通道 4624（成功登录）/ 4625（登录失败）/ 4740（账户锁定）；`type` 可选 all / logonFail / lockout。非管理员时**不执行查询**，返回 `admin:false` + `degraded:true` + notice；仅覆盖本地/工作组，域环境 Kerberos 失败（4771）不在范围
7. `query`：白名单外的自定义查询——`ids`（数组，下推过滤）+ `level` + `provider`（正则，1 秒超时保护）+ `msg`（子串）+ `logName`（单通道，默认 System+Application）；省略 `level` = 全级别；过滤词过宽时耗时随命中量增长（30 秒超时兜底）
8. `detail`：`recordId`（直取记录号，取 events 样本里的值）或 `id`（取该 ID 最近一条）二选一，`logName` 必传（样本里的 `logName` 字段）；返回完整原文（保留换行，20k 字符封顶并标记截断），查无此记录时 notice 说明

## 白名单边界

各 scope 的 ID 清单是「相关事件」清单而非全部故障事件，覆盖不到的冷门故障走 `query`。

## 通用约定

1. `notice` 是命中概况、截断与降级附注、字段导读，转达给队员时不能省略
2. `error` 表示查询失败并附原因，如实转达；`unreadable > 0` 不是失败，其余记录正常返回
3. 消息文本随系统语言本地化（中文系统出中文消息）；`levelName` 固定英文映射不受影响
