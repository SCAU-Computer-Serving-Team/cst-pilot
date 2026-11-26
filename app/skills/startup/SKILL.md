---
name: startup
description: startup 开机自启盘点：注册表自启项、启动文件夹与自动启动服务。
---

# startup

只读开机自启盘点工具，不接受输入参数（传 `{}`）。返回结构为 `{ "startup": { ... }, "elapsedMs": N }`。

## 场景选择

| 目标 | 调用与联查 | 结束条件 |
|---|---|---|
| 盘点开机自启项与自动服务 | `startup({})` | 列出可疑或目标条目，标注 `disabled` 状态与未覆盖机制 |
| 核实可疑条目是否正在运行 | 联查 `sys({ scope: "proc" })` | 匹配到进程 `path` 可作为正在运行的正面旁证；未入榜或 `path: null` 须标“未核实”，不可断定未运行 |
| 关闭自启项或停用第三方服务 | 转 `runbook` 编写修复命令 | 服务 `path` 被截断或为 `null` 时，不可直接复制拼命令 |

## 返回字段与口径

| 字段 | 内容与边界 |
|---|---|
| `regItems[]` | HKLM 与 HKCU 的 `Run`、`RunOnce` 及 HKLM `Wow6432Node\Run`：`source`、`name`、`command`、`disabled` |
| `startupFolders[]` | 当前用户（`scope: "user"`）与所有用户（`scope: "allUsers"`）启动文件夹：`scope`、`path`、`items[]: { name, disabled }`。仅列直接文件名，不展开快捷方式目标；目录不存在时可省略对应对象 |
| `services[]` | 自动启动服务（含延迟自启）：`name`、`display`、`state`（`Running` 排前）、`path`（超 140 字符截断附 `…`）。同时保留系统与第三方服务，`System32` 路径不能单独证明归属 |
| `notice` | 采集口径与未覆盖范围说明 |
| `degraded?` / `collectionErrors?` | 原生采集失败或回退时出现；原生服务接口失败但回退成功后，仍可同时包含 `services` 与 `collectionErrors`，按错误 `source` 判断受影响部分 |

## 状态边界与超时规则

1. **`disabled` 三态**：`true` 为已禁用，`false` 为已启用，`null` 为未取得可用状态或该项不判定（`RunOnce` 固定为 `null`，状态记录缺少首字节时也为 `null`）。条目存在或为 `false` 均不保证实际启动成功。
2. **未覆盖入口**：不包含计划任务与 WMI 事件订阅，排查持久化问题须向队员说明该边界。
3. **错误与超时**：超时上限 30 秒，超时或快照主入口失败返回 `isError: true`（`{"error", "elapsedMs"}`）。子来源失败可保留在 `degraded: true` 与 `collectionErrors` 中，此时空数组不可解释为无自启项。
