---
name: disk
description: disk 只读磁盘诊断参考。排查分区容量（space）、硬盘型号与逻辑卷（info）、SMART 可靠性（health）、目录与大文件占用排行（usage）或前三项聚合（all）时查阅。
---

# disk

只读磁盘信息工具，全程零临时文件落盘。按 `scope` 返回对应字段，结合 `notice`、错误数组与 `degraded` 判读采集边界。

## 参数

| 参数 | 适用 `scope` | 约束与默认行为 |
|---|---|---|
| `scope` | 全部（必填） | `space`、`info`、`health`、`usage`、`all` |
| `drive` | `space` / `info` / `health` / `all` | 可选，仅接受单盘符（如 `C` 或 `C:`）；省略查全部 |
| `path` | `usage`（必填） | 目标目录或盘符；传裸盘符 `C:` 按盘根 `C:\` 解析，统计整棵目录树 |
| `top` | `usage` | 可选，排行条数，默认 `20`，上限 `100` |

## 各 scope 来源、降级与判读

1. **`space`**：查询各逻辑卷剩余容量（网络卷独立短超时隔离）。返回 `space[]`：`drive`、`totalGB`、`freeGB`、`usedPct`（二进制 GiB，`bytes / 2^30`）。`freeGB: 0` 表示零剩余。
2. **`info`**：查询盘符与物理盘映射、逻辑卷及物理硬盘信息。指定 `drive` 且盘符关联失败时，`physicalDisks` 保留全量设备并附 `infoNotice` 与 `degraded: true`。
3. **`health`**：优先通过原生存储接口读取 NVMe 与 ATA SMART 健康度，受限时回退系统存储可靠性计数器。输出 `smart[]` 固定 6 项键：`DeviceId`、`Wear`、`Temperature`、`PowerOnHours`、`ReadErrorsTotal`、`WriteErrorsTotal`。`smart: null` 表示未取回数据，不能据此判断健康或故障；仅在出现访问拒绝时提示管理员权限，部分失败见 `smartErrors`。
4. **`usage`**：管理员权限下的本地 NTFS 根目录默认建立整卷索引（`method: "native-mft"`），完整快照写入 120 秒卷缓存供 `ls` 复用。子目录、非管理员、非 NTFS、UNC 路径或整卷索引不可用时，走无条目上限的原生并行目录遍历（`method: "native-walk"`，受外层超时与取消信号控制）。仅整卷索引失败回退时附 `degradedFrom`。
5. **`all`**：一次返回 `space`、`info` 与 `health`，不含 `usage`。

## usage 字段与统计口径

| 字段 | 含义与口径 |
|---|---|
| `root` / `totalGB` | 扫描根路径与整树大小（`topDirs[].pct` 以 `totalGB` 为分母） |
| `topDirs` | 目录大小排行，含不同深度，父子目录不可相加 |
| `topFiles` | 单文件排行：`path`、`sizeGB`、UTC `modified` |
| `extAgg` | 按扩展名聚合字节与文件数，最多 40 项 |
| `staleFiles` | `≥ 50 MiB` 且 `≥ 1 年`未修改的大文件，大者优先 |
| `sizeGB` 精度 | 保留两位小数；真实非零值两位归零时自动升至 4 或 6 位 |

统计保护规则：

1. 本地 NTFS 执行确定性硬链接去重，跨目录硬链接只归属一个确定路径。
2. 跳过符号链接与重解析目录；完全驻留云文件计入逻辑大小，在线或部分驻留云占位文件按本地分配量计入且不下钻云占位目录。
3. 工具包敏感路径名在排行与拒绝列表中隐去，其体积仍计入 `totalGB`。
