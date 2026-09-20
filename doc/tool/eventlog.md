# eventlog：Windows 事件日志

查询崩溃、蓝屏、服务、磁盘和登录历史，并读取单条原文。实时负载使用 [sys](sys.md)，硬盘自身可靠性使用 [disk health](disk.md#healthsmart-可靠性数据)。

实现：[eventlog.ts](../../agent/home/extensions/diagnostics/eventlog.ts) 与 [eventlog-core.ts](../../agent/home/extensions/diagnostics/eventlog-core.ts)；设计见 [事件日志设计](../design/tool/event-design.md)。

## 调用

```js
eventlog({ scope: "recent", hours: 48 })
eventlog({ scope: "boot", kind: "bluescreen" })
eventlog({ scope: "crash", app: "example.exe" })
eventlog({ scope: "query", logName: "System", ids: [41], top: 20 })
eventlog({ scope: "detail", logName: "System", recordId: 154433 })
```

| scope | 查询范围 | 专用参数 |
|---|---|---|
| `recent`（默认） | System + Application 错误、警告 | level |
| `boot` | System 启停、意外关机、蓝屏和 WHEA | kind |
| `crash` | Application 崩溃、无响应、启动失败 | app |
| `service` | System 的服务控制管理器故障 | name |
| `disk` | System 磁盘、文件系统与掉盘事件 | — |
| `security` | Security 登录审计 | type |
| `query` | 自定义查询，默认 System + Application | logName、ids、level、provider、msg |
| `detail` | 单条记录原文 | logName 必填；recordId 或 id 二选一必填 |

### 参数

| 参数 | 默认值或范围 | 语义 |
|---|---|---|
| `hours` | 默认 24，最大 720 | 查询时间窗；detail 不使用 |
| `top` | 默认及最大 100 | 显示的事件数；detail 不使用 |
| `level` | warn / error | warn 包含 Critical、Error、Warning；error 包含 Critical、Error。recent 默认 warn，query 省略时不限级别 |
| `kind` | all / unexpected / bluescreen | boot 默认 all |
| `type` | all / logonFail / lockout | security 默认 all |
| `app` / `name` / `msg` | 字符串 | 消息字面子串，不区分大小写 |
| `ids` | 0–65535 的整数数组 | query 的事件 ID，0 合法 |
| `provider` | 正则字符串 | query 提供程序匹配，忽略大小写，单次匹配超时 1 秒 |
| `logName` | 通道名 | 仅 query/detail 接受；不覆盖其他 scope 的固定通道 |
| `recordId` / `id` | 记录号 / 事件 ID | 二选一：recordId 精确定位记录，id 取最近一条；同时传入会报错 |

## 返回

结果位于对应 scope 对象中，例如 `result.recent`。

| 字段 | 含义 |
|---|---|
| `logs`、`hours`、`top`、`level` | 实际查询范围与参数回显；level 按查询出现 |
| `total` | 当前日志中符合条件、成功处理并去重的记录数 |
| `events[]` | 最新 top 条，时间倒序 |
| `truncated` | events 未展示全部命中记录 |
| `counts[]` | 按 provider/ID 计数，含 key、provider、id、n、last，最多 100 组 |
| `countsTruncated` | 计数分组超出 100 组 |
| `unreadable` | 枚举期间无法读取的记录数 |
| `noMatch` | 零命中或提供程序不匹配提示码 |
| `firstTime` / `lastTime` | **返回事件样本**最早和最新时间；不是整个时间窗的覆盖保证 |
| `admin`、`notice` | 权限和解释信息 |

每条事件包含 `logName`、`time`（本地时间）、`recordId`、`level`、固定英文 `levelName`、`provider`、`id`、`msg`。msg 为最多 200 字符的简述，无渲染文本时可为 `null`。

只有 `countsTruncated=false` 时，才要求 `sum(counts.n)=total`。计数表覆盖命中记录，不只是 events 样本。

### detail

用样本的 logName + recordId 读取同一条记录；按 id 则取该事件 ID 最近一条。找到时返回 found=true、事件字段及 machine；原文保留换行，超过 20,000 字符时截断并在 notice 说明。记录可能已被日志轮转清除，此时返回 found=false 和说明。

## 内置事件范围

| scope | ID / 提供程序 |
|---|---|
| boot all | 12、13、6005、6006、6009；41、6008、1001；1074、19、7045；另合并 Microsoft-Windows-WHEA-Logger |
| boot unexpected | 41、6008 |
| boot bluescreen | 1001 + WHEA-Logger |
| crash | Application Error/1000、.NET Runtime/1026 限 Error 档；Windows Error Reporting/1001、Application Hang/1002、SideBySide/33、35 不限级别 |
| service | SCM：7000、7001、7002、7003、7013、7038、7041、7009、7011、7022、7023、7024、7031、7032、7034、7043、7025、7026 |
| disk | 7、11、51、129、153、55、98、50、140、157 |
| security | 4624 登录成功、4625 登录失败、4740 锁定；logonFail 只取 4625，lockout 只取 4740 |

1001 在 System 中用于蓝屏线索，在 Application 中是 WER 报告，须结合通道和提供程序。7045 是新装服务记录，位于 boot，不属于服务运行失败。事件 ID 清单只是排查入口，不能单凭 ID 判定根因；其他情况使用 query。

## 错误与限制

- Security 先检查管理员身份。非管理员返回 `admin:false`、`degraded:true` 和说明，不把无权限伪装成零事件；身份探测失败不缓存为 false。
- 枚举中无法读取的事件会被跳过并计数；缺失通道等结构性错误返回 error。不同语言系统按错误 ID 和异常类型分类，不解析本地化错误文本。
- provider 正则非法或超时会报错；消息子串不带正则语义。多组查询共用相同后置条件，crash 的 app 过滤对全部分组生效。
- 返回数量有上限，但统计仍可能遍历大量记录。宽泛 query 或消息过滤可能触发 30 秒超时，不能把 top 当成扫描预算。
- 日志轮转可能已清除窗口内的旧记录。样本时间集中只能提示突发或留存不足，不能据此断言全部窗口完整，也不能直接用 total ÷ hours 计算真实发生速率。
- Security 仅覆盖上述本地/工作组事件，不覆盖 Kerberos 4771 等域审计。工具不清理、导出文件或修改日志配置。
