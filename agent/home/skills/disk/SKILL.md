---
name: disk
description: disk 只读磁盘诊断参考。排查分区容量（space）、硬盘型号与逻辑卷（info）、SMART 可靠性（health）、目录占用排行（usage）或前三项聚合（all）时查阅。
---

# disk

只读磁盘信息工具，无临时文件落盘。按 `scope` 返回对应数据，结合 `notice`、错误数组与 `degraded` 判读采集边界。

## 参数

| 参数 | 适用 `scope` | 约束与默认行为 |
|---|---|---|
| `scope` | 全部（必填） | `space`、`info`、`health`、`usage`、`all` |
| `drive` | `space` / `info` / `health` / `all` | 可选，单盘符（如 `C` 或 `C:`）；省略查全部 |
| `path` | `usage`（必填） | 要统计的目录或盘符，遍历整棵目录树 |
| `top` | `usage` | 可选，目录排行条数，默认 `20`，上限 `100` |

## 各 scope 来源与返回

1. **`space`**：Node `statfsSync` 统计各卷。返回 `drive`、`totalGB`、`freeGB`、`usedPct`。
2. **`info`**：PowerShell 查询 `Get-PhysicalDisk` 与 `Win32_LogicalDisk`。指定 `drive` 且盘符关联失败时保留全量 `physicalDisks` 并附 `infoNotice` 与 `degraded: true`。
3. **`health`**：PowerShell 查询 `Get-StorageReliabilityCounter`。`smart: null` 不能用于判断健康状态；仅出现访问拒绝时提示管理员权限，部分失败见 `smartErrors`。
4. **`usage`**：Node 逐文件递归统计（`method: "node-walk"`，50 万条目预算），输出 `root`、`totalGB`、`topDirs`、`stats`、`truncated` 与 `denied`。触发预算或目录拒绝访问后结果为下界。
5. **`all`**：一次返回 `space`、`info` 与 `health`，不含 `usage`。

## 通用约定

1. `sizeGB` 默认保留两位小数，非零极小值自动升至 4 或 6 位。
2. `notice` 说明统计范围与截断限制，不作为故障结论。
