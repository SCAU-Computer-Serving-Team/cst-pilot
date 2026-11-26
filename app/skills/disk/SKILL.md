---
name: disk
description: disk 磁盘诊断参考。排查分区容量（space）、物理盘与逻辑卷（info）、SMART 健康度（health）、目录与大文件占用（usage）或前三项聚合（all）时查阅。
---

# disk

只读磁盘诊断工具，不产生临时文件。

## 排查路径

1. 磁盘空间告急时，先调 `space` 确认满盘盘符，再调 `usage` 找大目录与大文件，逐层核查子目录转 `ls`。
2. 怀疑坏道、掉盘或硬盘老化时，调 `info` 与 `health`（或一次调 `all`），结合 `eventlog(disk)` 与 `sys(io)` 交叉验证。
3. 每次返回后核对顶层 `elapsedMs`、`degraded`、`notice` 及全部错误数组。确认异常来源与缺失边界后结束排查。

## 参数

| 参数 | 适用 `scope` | 约束与默认值 |
|---|---|---|
| `scope` | 全部（必填） | `space`、`info`、`health`、`usage`、`all` |
| `drive` | `space` / `info` / `health` / `all` | 可选，单盘符（如 `C` 或 `C:`）；省略查全部 |
| `path` | `usage`（必填） | 目录或盘符；裸盘符 `C:` 按 `C:\` 解析，不可为符号链接或重解析点 |
| `top` | `usage` | 可选，排行条数，默认 `20`，上限 `100` |

## 各 scope 返回字段与判读

| `scope` | 顶层字段 | 关键口径与边界 |
|---|---|---|
| `space` | `space[]` | `drive`、`totalGB`、`freeGB`、`usedPct`（单位 GiB）。`freeGB: 0` 表示零剩余；总容量为 0 时 `usedPct` 为 `null` |
| `info` | `physicalDisks[]`、`volumes[]`、`infoNotice?` | `physicalDisks` 含型号、序列号、介质类型、总线、健康与运行状态、`DeviceId`、`sizeGB`；`volumes` 含 `drive`、`label`、`fs`、`driveType`、`totalGB`、`freeGB`。指定 `drive` 且盘符映射失败时保留全量 `physicalDisks` 并附 `infoNotice` 与 `degraded: true` |
| `health` | `smart`、`smartNotice`、`smartErrors?` | `smart` 为数组或 `null`，固定 6 键：`DeviceId`、`Wear`、`Temperature`、`PowerOnHours`、`ReadErrorsTotal`、`WriteErrorsTotal`。单次上限 256 块物理盘。指定 `drive` 映射失败时停止查询（`smart: null` 并附 `smartErrors`）。`smart: null` 表示未取回数据，不能判断健康或故障；仅出现访问拒绝时提示管理员权限 |
| `usage` | `usage` | 见下方 `usage` 字段表 |
| `all` | 同层合并 | 一次返回 `space`、`info`、`health` 全部字段，不含 `usage` |

## usage 字段与扫描口径

| 字段 | 含义与判读 |
|---|---|
| `method` | `native-mft`（管理员本地 NTFS 盘根整卷索引，写入 120 秒卷缓存供 `ls` 复用）或 `native-walk`（子目录、普通权限、非 NTFS、网络路径或索引回退后的并行目录遍历） |
| `root` / `totalGB` | 扫描根路径与整树大小；`topDirs[].pct` 以 `totalGB` 为分母，结果不完整时 `pct` 为 `null` |
| `topDirs[]` | `path`、`sizeGB`、`pct`；含不同深度目录，父子目录体积不可相加 |
| `topFiles[]` | `path`、`sizeGB`、UTC 时间 `modified` |
| `extAgg[]` | 按扩展名聚合 `ext`、`files`、`sizeGB`，最多返回 `min(top, 40)` 项 |
| `staleFiles[]` | 至少 `50 MiB` 且 1 年未修改的文件，按大小降序，含 UTC 时间 `modified` |
| `stats.filesScanned` | 已扫描文件数；`stats` 不包含耗时字段 |
| `truncated` / `denied[]` | 存在未统计完整目录时 `truncated: true`，`denied` 记录受限路径；此时体积仅为下界 |
| `degraded` / `degradedFrom` | 整卷索引失败回退或触及预算上限时为 `true`；回退原因见 `degradedFrom` |

统计保护规则：

1. `sizeGB` 默认保留两位小数，极小非零值自动提高小数位，避免被误读为零。
2. 本地 NTFS 执行确定性硬链接去重，跨目录硬链接只计入一个确定路径。
3. 跳过符号链接与重解析目录。完全驻留云文件计逻辑大小，在线或部分驻留云占位文件按本地分配量计入且不下钻云占位目录。
4. 工具包敏感路径名在排行与 `denied` 中隐去，体积仍计入 `totalGB`。

## 耗时与错误判读

1. 顶层统一返回整数毫秒 `elapsedMs`。`space`、`info`、`health` 超时上限 30 秒，正常数秒内返回；`elapsedMs` 明显偏高提示慢盘、休眠盘或无响应网络卷。`usage` 超时上限 215 秒。
2. `isError: true` 表示整次调用失败（返回 `{"error", "elapsedMs"}`），直接转达原因。
3. `degraded: true` 表示局部降级，须结合 `collectionErrors`、`smartErrors`、`degradedFrom` 与 `notice` 解释，禁止把空数组或 `null` 当作健康或零值。
