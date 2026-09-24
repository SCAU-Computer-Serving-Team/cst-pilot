---
name: disk
description: disk 工具的参数与返回字段说明。覆盖 scope 枚举含义、参数约束、各 scope 的数据来源、耗时与降级行为、返回字段定义。
---

# disk 工具说明

只读磁盘信息工具。按 `scope` 返回查询结果，不改系统配置或机主文件；扫描会在工具包 `wiztree/tmp` 写临时 CSV，并在结束时尝试清理。

## 参数

1. `scope`（必填）：`space` / `info` / `health` / `usage` / `all`
2. `drive`（可选）：传 `C` 或 `C:` 等盘符，对 `space` / `info` / `health` / `all` 生效。省略则查询全部；`info` 按盘符过滤卷及关联物理盘，关联失败时保留全量物理盘并通过 `infoNotice` 说明
3. `path`（可选）：`scope=usage` 时必填，要分析的目录或盘符，整个目录树会被统计
4. `top`（可选）：`scope=usage` 时生效，排行条数，默认 20，上限 100

## 各 scope 的数据来源、耗时与返回

1. `space`：Node `statfsSync` 统计各卷，瞬间返回。返回字段：`drive` / `totalGB` / `freeGB` / `usedPct`
2. `info`：pwsh 查询 `Get-PhysicalDisk` 与 `Win32_LogicalDisk`。`physicalDisks` 含型号 / 序列号 / SSD 或 HDD / NVMe 或 SATA 或 USB / 健康状态；`volumes` 含盘符 / 卷标 / 文件系统 / 盘类型（数字码已译为 Fixed / Removable / Network / Optical）/ 总量 / 剩余
3. `health`：pwsh 查询 `Get-StorageReliabilityCounter`，返回设备支持的磨损度、温度、通电小时与读写错误。可用字段取决于设备、驱动、桥接方式和权限；只有实际权限拒绝才建议提权。`smart: null` 不能用于判断健康状态，部分失败见 `smartErrors`
4. `usage`：优先用随包 WizTree 扫描并流式解析。NTFS 标为 `wiztree-mft`，其他文件系统标为 `wiztree-walk`；探测失败会说明。WizTree 不可用或失败时改用 Node 递归统计，只保证目录排行，方法为 `node-walk`；有失败原因时附 `degradedFrom`。扫描耗时取决于规模、权限和介质，预算停止后的大小为下界
5. `all`：一次执行 `space` + `info` + `health`，不含 `usage`

## usage 返回字段定义

1. `root`：所查路径；`totalGB`：整树大小（各项 `pct` 均相对它计算）
2. `topDirs`：目录大小排行，含 `pct`
3. `topFiles`：单个大文件排行，含 `pct`
4. `extAgg`：按扩展名聚合，含文件数
5. `staleFiles`：≥50MB 且 ≥1 年未修改的文件，大者优先
6. `sizeGB` 自适应精度：真实数据小到两位小数归零时自动提升位数，不显示假 0

## 通用约定

1. 结果中的 `notice` 字段是降级/附注说明，转达给队员时不能省略
2. WizTree 扫描结果有秒级时效偏差
3. WizTree 扫描数据同时写入进程内共享缓存，供 `ls` 复用已扫描路径
