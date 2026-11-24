---
name: disk
description: disk 工具的参数与返回字段说明。覆盖 scope 枚举含义、参数约束、各 scope 的数据来源、耗时与降级行为、返回字段定义。
---

# disk 工具说明

只读磁盘信息工具。按 `scope` 返回查询结果，不修改系统配置或机主文件，全程零临时磁盘文件写入。

## 参数

1. `scope`（必填）：`space` / `info` / `health` / `usage` / `all`
2. `drive`（可选）：传 `C` 或 `C:` 等盘符，对 `space` / `info` / `health` / `all` 生效。省略则查询全部，`info` 按盘符过滤卷及关联物理盘，关联失败时保留全量物理盘并通过 `infoNotice` 说明
3. `path`（可选）：`scope=usage` 时必填，要分析的目录或盘符（传 `C:` 时按盘根 `C:\` 解析），整个目录树会被统计
4. `top`（可选）：`scope=usage` 时生效，排行条数，默认 20，上限 100

## 各 scope 的数据来源与返回

1. `space`：Win32 `GetDiskFreeSpaceExW`（带线程防弹窗与网络卷超时隔离）统计各卷。返回字段：`drive` / `totalGB` / `freeGB` / `usedPct`
2. `info`：优先 Win32 `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS` 关联盘符与物理盘（全 extent 校验通过时采用，否则回退 WMI）、Win32 原生接口枚举逻辑卷，并结合 `MSFT_PhysicalDisk`（Storage WMI 缺失时回退 SetupAPI 与存储 IOCTL）返回 `physicalDisks` 与 `volumes`
3. `health`：优先 Win32 原生存储 IOCTL（NVMe 健康日志 Page `0x02` 与只读 ATA SMART 查询，交叉核验序列号并严格输出 `API.md` 既定 6 项指标），不支持或身份不匹配的控制器回退单次批量 `MSFT_StorageReliabilityCounter` WMI 查询。可用字段取决于设备、驱动与权限，只有实际权限拒绝才建议提权。`smart: null` 不能用于判断健康状态，部分失败见 `smartErrors`
4. `usage`：默认使用受控并发 Win32 `FileIdBothDirectoryInfo` 遍历（`method: "native-walk"`），均输出四类完整排行并做确定性硬链接去重与云占位保护。已提权的本地 NTFS 盘根在显式开启实验开关 `CST_ENABLE_MFT=1`（且未设 `CST_DISABLE_MFT=1`）时使用纯 Rust `ntfs-ng` 直读 `$MFT`（`method: "native-mft"`），失败回退遍历时附 `degradedFrom`
5. `all`：一次执行 `space` + `info` + `health`（复用同次原生卷探测），不含 `usage`

## usage 返回字段定义

1. `root`：所查路径，`totalGB`：整树大小（`topDirs[].pct` 相对它计算）
2. `topDirs`：目录大小排行，含 `pct`（包含不同深度，父子不可相加）
3. `topFiles`：单个大文件排行，含 `path`、`sizeGB` 与 UTC `modified`
4. `extAgg`：按扩展名聚合，含文件数，最多 40 项
5. `staleFiles`：≥50 MiB 且 ≥1 年未修改的文件，大者优先
6. `sizeGB` 自适应精度：真实数据小到两位小数归零时自动提升位数，不显示假 0

## 通用约定

1. `notice` 用于理解数据口径和采集限制
2. 实验开关开启时，整卷 `$MFT` 扫描结果同时写入进程内短有效期（120 秒）共享卷缓存，供 `ls` 复用子目录合计
