---
name: disk
description: disk 磁盘容量、硬盘识别与卷信息、SMART 健康度、整树占用排行。
---

# disk

只读磁盘诊断工具。

## 场景选择

| 现象 | 调用 | 结束条件 |
|---|---|---|
| 确认满盘盘符 | `disk({ scope: "space" })` | 列出各卷容量与剩余空间，或标明采集异常 |
| 找大目录与大文件 | 已知具体目录直接用 `ls`；需整树排行再调 `disk({ scope: "usage", path })` | 列出高占比路径、扫描方式与是否完整 |
| 核查硬盘、卷与健康度 | `disk({ scope: "info" })`、`disk({ scope: "health" })` 或 `disk({ scope: "all" })`；疑似坏道或掉盘联查 `eventlog(disk)` 与 `sys(io)` | 列出物理盘、卷与 SMART 读数，并标明缺失项与错误 |

## 参数

| 参数 | 适用 `scope` | 约束与默认值 |
|---|---|---|
| `scope` | 全部（必填） | `space`、`info`、`health`、`usage`、`all` |
| `drive` | `space` / `info` / `health` / `all` | 可选，单盘符如 `C` 或 `C:`；省略查全部 |
| `path` | `usage`（必填） | 目标目录或卷根，传 `C:` 或 `C:\`（传 `C` 会按相对目录解析）；不可为符号链接或重解析点 |
| `top` | `usage` | 可选，排行条数，默认 `20`，上限 `100` |

## 各 scope 返回与判读

顶层统一含 `elapsedMs`（整数毫秒）。非 `usage` 分支按子功能输出 `infoNotice`、`smartNotice`、`collectionErrors`、`smartErrors` 与 `degraded`，没有通用顶层 `notice`。

| `scope` | 返回字段 | 口径与边界 |
|---|---|---|
| `space` | `space[]` | `drive`、`totalGB`、`freeGB`（单位 GiB，四舍五入到 1 位小数）与百分比 `usedPct`（按原始字节计算）。`freeGB: 0` 无法区分精确为零或舍入为零；总字节数为 0 时 `usedPct` 为 `null` |
| `info` | `physicalDisks[]`、`volumes[]`、`infoNotice?` | `physicalDisks` 含 `FriendlyName`、`SerialNumber`、`MediaType`、`BusType`、`HealthStatus`、`OperationalStatus`、`DeviceId`、`sizeGB`；`volumes` 含 `drive`、`label`、`fs`、`driveType`、`totalGB`、`freeGB`。指定 `drive` 且物理盘关联失败时保留全量 `physicalDisks`，附 `infoNotice` 与 `degraded: true` |
| `health` | `smart`、`smartNotice`、`smartErrors?` | `smart` 为数组或 `null`，固定 6 键：`DeviceId`、`Wear`、`Temperature`、`PowerOnHours`、`ReadErrorsTotal`、`WriteErrorsTotal`；单项缺失或为 `null` 表示不支持或未采集。单次上限 256 块物理盘；部分 NVMe 控制器日志在多命名空间间共享计数。指定 `drive` 关联失败时停止查 SMART（`smart: null` 并附 `smartErrors`）。未取回数据可能返回降级对象且 `isError: false`，不可据此判健康；仅访问被拒时提示管理员权限 |
| `usage` | `usage` | 见下表，扫描字段、错误与 `notice` 均位于 `usage` 内 |
| `all` | 同层合并 | 合并 `space`、`info`、`health`，不含 `usage`；全无有效数据时仍可能返回降级对象而非 `isError: true` |

## usage 字段与扫描口径

| 字段 | 含义与判读 |
|---|---|
| `method` | `native-mft`（管理员本地 NTFS 卷根整卷索引，写入 120 秒卷缓存供 `ls` 复用）或 `native-walk`（子目录、普通权限、非 NTFS、网络路径或索引回退后的并行遍历） |
| `root` / `totalGB` | 扫描根路径与整树大小（GiB）。`usage` 内所有 `*GB` 默认保留 2 位小数，极小非零值自动提高小数位 |
| `topDirs[]` | `path`、`sizeGB`、`pct`（按原始字节计算，不可用已舍入的 `totalGB` 复算）。跨深度目录父子体积不可相加；总字节数为 0 或扫描不完整时 `pct` 为 `null` |
| `topFiles[]` | 始终为数组，每项含 `path`、`sizeGB`、UTC 时间 `modified` |
| `extAgg[]` | 始终为数组，含 `ext`、`files`、`sizeGB`，最多返回 `min(top, 40)` 项；扩展名种类超上限时 `usage.notice` 提示其为下界 |
| `staleFiles[]` | 始终为数组，至少 `50 MiB` 且 1 年未修改的大文件，含 UTC 时间 `modified` |
| `stats.filesScanned` | 已扫描文件数 |
| `truncated?` / `denied[]?` | 完整 `native-mft` 扫描不输出这两项。`native-walk` 遇链接、云占位或访问限制会置 `truncated: true`（此时可不含 `degraded`）。`denied` 最多记 50 条且隐去工具包敏感路径，空数组不证明扫描完整 |
| `degraded?` / `degradedFrom?` | 整卷索引失败回退或遍历触及预算上限时置 `true`，回退原因见 `degradedFrom`。仅发生索引回退不代表后续遍历也不完整，须结合 `usage.truncated` 与 `usage.notice` 判读 |
| `notice` | 扫描统计口径、截断与扩展名下界说明 |

保护与超时规则：

1. 本地 NTFS 执行确定性硬链接去重，跨目录硬链接只归属一个确定路径。
2. 跳过符号链接与重解析目录。完全驻留云文件计逻辑大小，在线或部分驻留云占位文件按本地分配量计入（分配量未知时跳过），不下钻云占位目录。工具包敏感路径名在排行与 `denied` 中隐去，体积仍计入 `totalGB`。
3. `space`、`info`、`health` 与 `all` 超时上限 30 秒；`usage` 外层超时 215 秒（索引阶段最多 180 秒）。超时或整次执行期失败返回 `isError: true`（`{"error", "elapsedMs"}`）；`usage` 超时或截断时缩小目标目录或改用 `ls` 逐级查看。
