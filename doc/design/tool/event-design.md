# eventlog 设计

状态：八个 scope 已实现。需求：[PRD R6](../../PRD.md)；接口与事件清单：[eventlog](../../tool/eventlog.md)。

## 目标与边界

读取历史故障线索，减少手动翻阅事件查看器的工作。工具只查询当前系统日志，不清理、修改配置或写出日志文件。sys 负责实时负载；disk SMART 反映设备可靠性，eventlog 反映系统记录的异常。

采用单工具多 scope：recent 默认入口，boot/crash/service/disk/security 面向固定场景，query 覆盖其他查询，detail 获取原文。各 scope 的参数独立；logName 只属于 query/detail，不能覆盖固定场景的通道。

## 查询流程

```mermaid
flowchart LR
    A[校验结构化参数] --> B[按组下推查询]
    B --> C[消息或提供程序后置过滤]
    C --> D[按通道和记录号去重]
    D --> E[统计全部命中]
    E --> F[返回最新样本和计数表]
```

数据源为便携 pwsh 的 Get-WinEvent。FilterHashtable 下推 LogName、Level、StartTime、Id、ProviderName；组内 AND、组间 OR。boot 的 ID 与 WHEA 可能重叠，按 logName/recordId 去重后再取全局 top。

消息过滤使用 OrdinalIgnoreCase 子串匹配；provider 正则使用 IgnoreCase 和 1 秒 MatchTimeout。多个分组只允许使用相同后置条件，确保 crash.app 能覆盖全部分组。

## 事件范围

| 场景 | 设计依据与边界 |
|---|---|
| boot | 启停、意外关机、蓝屏、更新、新装服务和 WHEA；事件未必直接代表故障 |
| crash | 提供程序与 ID 配对；WER/Hang 保留原始级别，避免丢失 Information 级报告 |
| service | SCM 故障白名单；排除普通状态变化和一次性配置通知 |
| disk | 坏块、控制器、文件系统、超时重试和掉盘线索 |
| security | 4624/4625/4740；不覆盖域 Kerberos 4771 等事件 |
| query | ID、级别、通道和后置条件组合，补充固定清单之外的事件 |

清单源于项目原有微软排查资料和 SCM 提供者表核对，不应当作“全部故障事件”目录。完整 ID 保留在 [工具文档](../../tool/eventlog.md#内置事件范围)，避免两处维护。

## 输出控制

| 规则 | 目的 |
|---|---|
| events 默认及最多 100 条 | 保留最新样本，限制输出 |
| counts 按 provider/ID 汇总，最多 100 组 | 看见重复事件的数量，避免逐条阅读 |
| 简述最多 200 字符 | 原文留给 detail |
| detail 最多 20,000 字符 | 防止单条长报告占满上下文 |
| total、truncated、countsTruncated | 明确样本和统计表是否完整 |
| firstTime/lastTime | 说明返回样本的时间跨度，不承诺日志覆盖整个窗口 |

只有计数表未截断时，sum(counts.n) 才应等于 total。输出有界不等于枚举成本恒定：消息过滤仍需读取命中记录，宽泛查询可能超时。

## 失败处理与安全

| 决策 | 原因 |
|---|---|
| SilentlyContinue 枚举后分类 $Error | 个别 EventLogException 不应终止全部查询；跳过数量需要返回 |
| 按异常类型与 FQID 分类 | 不依赖本地化错误文本 |
| 区分零命中、提供程序不匹配、缺失通道 | “没有事件”和“无法查询”含义不同 |
| Security 显式检查管理员身份 | 历史实测无权限时可能静默零命中；探测失败不缓存为非管理员 |
| 动态字符串编码为数据 | 防止 ASCII/弯引号改变脚本语法 |
| detail 使用固定 EventRecordID XPath 模板 | FilterHashtable 不支持记录号；只允许校验后的整数进入模板 |

detail 的 XML 元素是 EventRecordID，PowerShell 属性是 RecordId。事件 ID 0 合法；System/1001 与 Application/1001 需要按通道区分。

## 验证

按[测试指南](../../test/README.md)执行 eventlog 公共组，重点覆盖权限、过滤、计数与截断。核对 SCM 元数据时需解码事件 ID 的高位编码；具体环境结果放在 `doc/test/testlog/`。
