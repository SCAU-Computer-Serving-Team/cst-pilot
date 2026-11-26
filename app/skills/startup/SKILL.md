---
name: startup
description: startup 开机自启盘点参考。核查注册表自启项、启动文件夹、自动启动服务及 disabled 三态边界时查阅。
---

# startup

只读开机自启盘点工具，无输入参数，一次采集三类自启配置。

## 排查路径

1. 排查开机慢、后台自启弹窗或残留软件时调用 `startup({})`。
2. 发现可疑条目后，结合 `sys(proc)` 的 `path` 核实是否正在运行。
3. 需要关闭自启项或停用第三方服务时，将操作写入 `runbook` 交队员手动执行。
4. 核对三类清单、`disabled` 三态、`degraded`、`collectionErrors` 与顶层 `elapsedMs`，同时向队员交代未覆盖范围后结束排查。

## 参数与返回字段

参数：`{}`（不接受任何字段）。返回结构为 `{ "startup": { ... }, "elapsedMs": N }`。

| 字段 | 内容与口径 |
|---|---|
| `regItems[]` | 注册表 `Run`、`RunOnce`（HKLM 与 HKCU）及 HKLM `Wow6432Node\Run`：`source`、`name`、`command`、`disabled` |
| `startupFolders[]` | 当前用户（`scope: "user"`）与所有用户（`scope: "allUsers"`）启动文件夹：`scope`、`path`、`items[]: { name, disabled }` |
| `services[]` | 自动启动服务（含延迟自启）：`name`、`display`、`state`（`Running` 排前）、`path`（超 140 字符截断附 `…`） |
| `notice` | 采集口径与未覆盖范围说明 |
| `degraded` / `collectionErrors` | 部分来源失败时出现 |

## disabled 三态与能力边界

1. **`disabled` 三态**：`true` 表示任务管理器启动开关已禁用；`false` 表示已启用；`null` 表示无匹配的禁用状态记录。`RunOnce` 不套用同名 `Run` 的禁用记录。条目存在或状态为 `false` 均不保证实际启动成功。
2. **未覆盖机制**：不包含计划任务与 WMI 事件订阅等持久化入口，排查顽固自启时须向队员说明该边界。
3. **服务清单口径**：同时保留系统与第三方自动启动服务，不按厂商过滤；`System32` 路径不能单独证明归属或可信度。
4. **耗时与错误**：超时上限 30 秒；`elapsedMs` 明显偏高提示服务管理器或系统查询响应慢。整次失败返回 `isError: true`；`degraded: true` 时空数组不能解释为没有自启项。
