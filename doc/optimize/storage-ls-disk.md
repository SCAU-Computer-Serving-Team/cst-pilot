# `ls` 与 `disk` 存储诊断重构优化

本文记录阶段 9.3 阶段 1 中 `ls` 与 `disk` 存储诊断工具的底层重构背景、架构决策与实测对比。架构决策见 [ADR-014](../ADR.md#adr-014移除-wiztree存储诊断改用纯-rust-mft-与-win32-ioctl)，工程实施清单见 [9.3.optimization.md](../rust/plan/9.3.optimization.md)。

## 一、背景

重构前，`ls` 与 `disk` 在许可合规、便携介质安全、采集延迟和统计口径四方面存在瓶颈：

1. **第三方商用许可与体积负担**：随包分发的 `wiztree/` 目录共占 `18.83 MiB`（`WizTree64.exe` `10.46 MB`、`WizTree.exe` `7.02 MB`、61 个语言包 `1.34 MB`）。官方分发许可规定免费版仅限个人使用，随应用对外分发须年付 US$1,200 商业订阅。
2. **临时文件写盘与解析低效**：每次全盘扫描先向 `.state/tmp/wiztree/` 写入 `3.1–40.8 MB` 临时 CSV（整盘可达数百 MB），在只读或写保护 U 盘上直接失败。旧解析器每行重建 `csv::ReaderBuilder`，解析 `WinSxS` 的 `40.8 MB` CSV 耗时 `5,177 ms`；旧缓存以 `HashMap<String, u64>` 存储全大写路径，百万目录易触发 `128 MiB` 内存上限。
3. **单线程遍历耗时高且口径失真**：旧 `walk.rs` 单线程逐项调用 `read_dir` 与 `symlink_metadata`，扫描 `C:\Program Files` 需 `9.44 s`，扫描 `C:\Windows\WinSxS` 耗时 `30.04 s` 并逼近 30 秒截断。同时缺少硬链接去重与 OneDrive 云占位识别，容易造成空间虚高或触发云文件下载。
4. **WMI 通道覆盖不足与延迟高**：`disk` 通过 WMI 查询盘符映射耗时 `96.43 ms`；`disk(health)` 组合查询 `MSFT_PhysicalDisk` 与逐盘 SMART WQL 耗时 `186.54 ms`，且普通权限下返回 0 行健康指标。

## 二、决策

全仓移除 `WizTree` 调用与打包产物，改用纯 Rust 与 Windows 原生系统接口分层实现：

1. **移除 `WizTree` 并保留发布防线**：清理 Rust 工作区、Web 测试、`launcher.ps1`、`pack/` 与过渡扩展中的全部 `WizTree` 逻辑。`cst-updater` 与 `pack/release-checks.mjs` 保留对退役路径 `wiztree/`、`app/tools/wiztree/` 的发布拒收校验，以及旧版安装原位升级清退和失败回滚保障。
2. **纯 Rust `$MFT` 整盘直读（`ntfs-ng = "=0.1.2"`）**：管理员权限下的本地 NTFS 根目录默认启用 `$MFT` 直读（设置 `CST_DISABLE_MFT=1` 可关闭）。通过自定义投影采集 `Summary` 校验完整性，遇损坏、未映射记录或超 `512 MiB` 工作集即拒绝发布并自动回退目录遍历。合法快照复用 `DirectoryIndex` 写入进程内 `VolumeIndex` 缓存（120 秒有效，上限 4 卷 / `128 MiB`，按卷序列号与挂载信息防换盘串用），暂不接入 USN Journal 以避免修改系统日志。
3. **无条目上限的并行 Win32 目录遍历**：子目录、普通权限、非 NTFS 卷、UNC 路径与 `$MFT` 失败回退统一走 `FileIdBothDirectoryInfo` 并行遍历（不支持时回退 `FindFirstFileExW`）。生产路径取消人为条目数与 30 秒内层截断，统一受外层 215 秒超时与 `CancelSynchronousIo` 取消控制。全局最多 8 个工作线程（单次最多 6 个），在单次系统调用中同时取得逻辑大小、分配量、重解析标签与 `FileId`，完成跨目录确定性硬链接去重并跳过云占位目录与符号链接。
4. **`disk` 原生存储 IOCTL 分层回退**：`space` 使用线程错误模式守卫、`GetDriveTypeW` 与远程卷 2 秒短超时隔离；`info` 优先调用变长 `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS`（全量校验 extent）与 SetupAPI；`health` 在核验物理盘序列号后直读 NVMe Log `0x02` 与只读 ATA SMART，受限控制器回退单次批量 WMI `MSFT_StorageReliabilityCounter`。输出字段保持对齐 [API.md](../tools/API.md)。

## 三、实测数据与结论

### 3.1 测试环境

- **测试日期**：`2026-11-24`（全仓清理与文档收口 `2026-11-25`）。
- **主机配置**：16 逻辑核 Windows 11 主机，2 块 1 TB 512e NVMe SSD（`Samsung SSD 970 EVO 1TB`、`WD Blue SN580 1TB`），5 个未加密 NTFS 分区（`C:`–`G:`）。
- **测试口径**：Release 构建，目录遍历预热 OS 缓存后取 3 轮中位数，`disk` 接口取 20 轮端到端 P95。

### 3.2 目录遍历前后对比（`ls` / `disk(usage)`）

| 测试目录与规模 | 优化前单线程 `walk.rs` | 优化前 `WizTree64`（`/admin=0` + CSV） | 8 线程 `HandleIdBoth` 原型 | 生产 Release `native-walk` | 说明 |
|---|---:|---:|---:|---:|---|
| `crates/`（87 目录 / 506 文件） | `86.0 ms`（冷） | `3,246.9 ms` | `8.7 ms`（`9.9×`） | — | 较非提权 `WizTree64` 快 `373×` |
| `C:\Windows\System32`（1,560 目录 / 23,996 文件） | `1,770.8 ms`（冷 `7,430 ms`） | `1,995.2 ms`（含 `3.1 MB` CSV） | `44.0 ms`（`40.3×`） | `167.6 ms`（`10.6×`） | 原型去重 318 条硬链接 |
| `C:\Program Files`（8,302 目录 / 83,787 文件） | `9,443.1 ms`（冷 `19,420 ms`） | `4,913.8 ms`（含 `11.9 MB` CSV） | `316.2 ms`（`29.9×`） | `393.4 ms`（`24.0×`） | 达成 `≤ 500 ms` 目标，完整输出四类排行 |
| `C:\Windows\WinSxS`（107,223 目录 / 132,998 文件） | `30,036.9 ms`（触及 30 秒截断） | `21,283.5 ms`（含 `40.8 MB` CSV） | `4,534.5 ms`（`6.6×`） | `12.37 s`（`2.4×`） | 完整扫完 24 万项无截断；原型去重 14,298 条硬链接（`17.72 -> 16.35 GiB`） |

4 槽并发取消实测：触发取消后 `32.1 ms` 退出，紧邻的 `disk(space)` 在 `4.3 ms` 内恢复响应。

### 3.3 纯 Rust `$MFT` 引擎与热 `ls` 实测（`ntfs-ng v0.1.2`）

合成 8 线程解析压测：50 万记录（`488.3 MiB`）总耗时 `99.0–118.0 ms`（`424–505` 万记录/秒）；100 万记录（`976.6 MiB`）总耗时 `317.9–338.6 ms`（`295–315` 万记录/秒）。

管理员提权真实 NVMe 分区实测：

| 指标项 | 实测对象与规模 | 实测结果 | 备注 |
|---|---|---|---|
| `C:\` 冷扫（`native-mft`） | `2,123,652` 记录 / `1,647,375` 文件 / `214.80 GB` | `4.61–5.91 s` | 估算缓存 `58.26 MiB`，进程 Working Set `8.6 -> 97.5 MiB`，全周期峰值 `411.86 MiB`（Commit `444.33 MiB`） |
| 热 `ls`（`native-index`） | `C:\` / `C:\Windows` / `C:\Program Files` | `2.2–22.5 ms` | 全部命中卷缓存；`Program Files` 统计 `35.76 GB`，与提权 `walk` `35.8 GB` 一致 |
| `D:\` 取消与复扫恢复 | `418,508` 记录 / `355,687` 文件 / `174.70 GB` | 取消 `31.3 ms`，紧邻复扫 `1,333.3 ms` | 估算缓存 `7.98 MiB`，双卷常驻 Working Set `107.3 MiB` |

### 3.4 `disk` 原生 IOCTL 前后对比

| 能力项 | 优化前（WMI） | 优化后（Win32 IOCTL，Release 20 轮 P95） | 变化说明 |
|---|---:|---:|---|
| `disk(space)`（5 卷） | `1.27 ms` | `0.6 ms` | 叠加 `GetDriveTypeW` 与防弹窗守卫 |
| 盘符 → 物理盘映射 | `96.43 ms` | `0.19 ms`（隔离原型） | 提速 `499×`，替代正则解析 |
| `disk(health)`（2 块 NVMe） | `186.54 ms`（普通权限 0 行） | 非提权 `6.9 ms` / 提权 `10.2 ms` | 提速 `20×` 以上，普通权限直出 2 块盘 6 项核心健康字段 |
| `disk(info, C:)` / `disk(all, C:)` | — | `151.0 ms` / `118.7 ms` | Storage WMI `MSFT_PhysicalDisk` 在本机约 `84–151 ms` |

### 3.5 发行包体积与零落盘对比

| 指标 | 优化前 | 优化后（`xtask dist --profile release`） |
|---|---|---|
| 随包 `WizTree` 体积 | `18.83 MiB`（双架构可执行文件 + 61 语言包） | `0` 文件与目录，新增纯文本 `ntfs-ng` MIT 许可证 |
| 整包产物规模 | 含 `WizTree` 与临时目录 | 42 文件，解包 `105.76 MiB` / ZIP `43.14 MiB` |
| 扫描磁盘写入 | 每次写入 `3.1–40.8 MB` 以上临时 CSV | 零临时文件落盘，适配只读介质 |

### 3.6 结论与待补验证边界

本次重构消除了 `WizTree` 分发许可风险与临时文件写盘，将主流目录遍历压缩至百毫秒级，并在普通权限下打通 NVMe 健康度直读。本机环境为双 512e NVMe 未加密卷，4Kn 原生扇区盘、BitLocker 加密与锁定两态、SATA/USB-SAT/RST 控制器、真实断连 UNC 与热拔插换盘纳入阶段 `1E` 实机验收（见 [TASKS.md](../TASKS.md#阶段-93-分阶段执行清单)）。
