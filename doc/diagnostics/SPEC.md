# 诊断与执行工具规格

`diagnostics` 模块注册 8 个面向维修现场的工具（6 个只读查询工具，2 个交付／执行工具）。内部结构与共享缓存见 [ARCHITECTURE.md](ARCHITECTURE.md)，调用参数与返回字段见 [API.md](API.md)。

## 工具总览与通用规则

| 工具 | 类别 | 子功能（`scope`） | 职责边界 |
|---|---|---|---|
| `ls` | 查询 | 无 `scope` | 覆盖内置 `ls`，按大小倒序展示目录直接子项，支持逐层下钻空间占用 |
| `disk` | 查询 | `space`、`info`、`health`、`usage`、`all` | 卷空间、物理盘与逻辑卷、SMART 可靠性、目录／大文件／扩展名／陈旧文件占用排行 |
| `sys` | 查询 | `overview`、`proc`、`gpu`、`io`、`sensor` | 整机负载概况、进程 CPU／内存排行、显卡利用率与显存、磁盘与进程读写、可读传感器 |
| `startup` | 查询 | 无 `scope` | 注册表自启、启动文件夹与自动启动服务盘点，含禁用三态 |
| `eventlog` | 查询 | `recent`、`boot`、`crash`、`service`、`disk`、`security`、`query`、`detail` | Windows 事件日志场景化查询、按提供程序／ID 聚合计数与单条原文读取 |
| `driver` | 查询 | `problem`、`core`、`external`、`find` | 异常设备、网络／蓝牙／音频／显示核心硬件与驱动版本、外设与按条件查找 |
| `runbook` | 交付 | 无 `scope` | 在工具包 `outbox/<日期>/` 生成供队员手动复制执行的纯文本修复命令清单 |
| `bitlocker` | 执行 | `status`、`unlock`、`decrypt` | 实时查询卷加密状态、48 位恢复密码解锁、经本地确认发起后台完整解密 |

通用行为约束：

1. **只读与写入例外**：查询工具不修改注册表、系统配置、服务状态或机主文件；`disk usage` 与 `ls` 仅在 `wiztree/tmp` 写入临时 CSV 并在结束时清理；`runbook` 仅写 `outbox/`；只有 `bitlocker` 的 `unlock` 与 `decrypt` 经本地输入或确认后改动卷状态。
2. **口径与错误诚实**：工具固定说明（`notice`）仅陈述数据来源、单位、采样窗口与采集限制，不输出故障定性或修复建议。部分数据源失败时保留成功数据并标记 `degraded: true`；整次查询失败时抛错并由 Pi 标记 `isError`。空数组或 `null` 必须结合错误字段判读，禁止直接当作健康或零值。

## 查询工具规则

### ls 与 disk（存储分析）

- **ls 大小来源与预算**：优先读取进程内共享缓存（`wiztree-index`），缺失时触发全盘 WizTree 导出或回退 Node 递归统计（`walk`）。递归统计共用 **50 万条目、30 秒**预算，跳过符号链接与 junction；超限或遇到权限拒绝时停止并标注 `totalSize` 为下界，未知大小保持 `null`，任一子项不完整时全部 `pct` 置 `null`。UNC 路径直接走递归统计。
- **disk space / info / health**：容量按二进制 GiB（`字节 ÷ 2³⁰`）换算，`freeGB: 0` 表示剩余空间为零。指定 `drive` 时，若盘符与物理盘关联失败，`info` 返回未过滤的全量 `physicalDisks` 并附 `infoNotice`，`health` 则拒绝查询 SMART 并说明原因。SMART 仅在实际遭遇权限拒绝时提示管理员权限，`smart: null` 不代表硬盘健康或损坏。
- **disk usage 扫描与降级**：调用随包 WizTree（`/admin=0`）导出 CSV 并流式生成 `topDirs`（含不同深度，父子不可相加）、`topFiles`、`extAgg`、`staleFiles`（≥50 MB 且 ≥1 年未修改）。通过 `fsutil`（失败回退 CIM）探测实际卷类型：NTFS 标 `wiztree-mft`，其他标 `wiztree-walk`。WizTree 失败时降级为 `node-walk`（50 万次 stat 预算），此时**仅返回目录排行**。

### sys 与 startup（实时负载与启动配置）

- **sys proc / io 双采样**：`proc` 在同一 PowerShell 进程内间隔约 1.2 秒双采样，按 Stopwatch 实际间隔与逻辑核数计算 `cpuPct`，内存取工作集 `wsMB`；`io` 共用采样窗口读取物理盘忙碌度／队列长度（排除 `_Total`）与进程读写差分，两者口径不同不可直接相加。
- **sys gpu 聚合与补充**：每进程 `gpuPct` 取跨适配器最繁忙引擎利用率，**不累加并行引擎**；专用显存仅统计 >1 MB 的实例。并行查询 `nvidia-smi`（带 `uuid` / `pciBusId`），程序缺失或失败时由随包 LHM 补充；PDH 性能计数器路径按系统语言动态本地化，引擎查询失败自动重试一次。
- **sys sensor 能力边界**：零安装约束下不安装内核驱动（如 WinRing0 或 PawnIO），仅启用 LHM 的 CPU、GPU、主板用户态读数（过滤非有限值，上限 200 项）及 ACPI 热区（过滤 −50～150°C 外异常值）与逻辑核频率百分比。**低频率仅作排查线索，不能单独确认过热。**
- **startup 禁用三态**：采集 HKLM/HKCU 的 `Run`、`RunOnce`、Wow6432Node `Run`、用户与公共启动文件夹及 `StartMode='Auto'` 服务。关联 `StartupApproved` 首字节奇偶判读 `disabled`（`true` 已禁用 / `false` 已启用 / `null` 无匹配记录），严禁将 `Run` 的禁用状态套给同名 `RunOnce` 项。PE 下采集的是 PE 自身注册表与服务；计划任务和 WMI 事件订阅不在采集范围。

### eventlog 与 driver（历史日志与设备状态）

- **eventlog 场景与权限**：内置场景事件范围见 [API.md](API.md#eventlog-内置事件范围)。`boot` 合并系统启停与 WHEA 并按 `logName + recordId` 去重后再截断 `top`；`crash` 保留 Information 级 WER（1001）与 Hang（1002），`app` 子串过滤作用于全部分组；`7045` 归入 `boot` 新装服务而非 `service` 故障。`security` 执行前显式检查管理员身份，非管理员直接返回 `admin: false, degraded: true`，禁止将无权限伪装成零命中。
- **driver 枚举与查找**：`problem` 筛选 `Status=Error/Unknown`；`core` 网卡仅保留 `NetConnectionID` 非空项（含虚拟网卡），签名驱动版本仅按 `Net`、`Bluetooth`、`MEDIA`、`Display` 四类查询；`external` 覆盖 `USB*`、`BTHENUM*`、`DISPLAY*` 与可移动磁盘（可能含内置 USB 摄像头或内置屏）；`find` 要求至少传入一个条件，多个条件按 AND 组合，`id` 在 Node 侧匹配 `DeviceID` 与 `HardwareID` 数组。

## runbook：命令清单交付

1. **文件组织与序号**：写入 `outbox/<YYYY-MM-DD>/<序号>-<风险档中文>-<标题>.txt`，序号范围 `01`–`999`，当天唯一递增、跨天从 `01` 重置。传入已有 `sequence` 时原地替换该序号文件（文件名随新标题更新并清理旧文件名，文件总数不增）。
2. **格式与安全限制**：UTF-8 BOM + CRLF，文件头一行，每条命令四行（环境、说明、`命令：`、单行命令正文），条目间空行分隔，无 Markdown。命令含换行、标题或说明为空、超长或序号不存在时整次调用失败且不留半成品文件。跨进程通过日期目录命名管道锁互斥写入。

## bitlocker：状态、解锁与解密

| 操作 | 行为与安全规则 |
|---|---|
| `status` | 实时调用 `Win32_EncryptableVolume` 方法查询指定盘或全部卷，不使用缓存快照 |
| `unlock` | 仅支持 48 位数字恢复密码（`UnlockWithNumericalPassword`）。已解锁卷直接跳过；提交前复核卷标识与锁定状态，调用后通过 `GetLockStatus()` 确认解锁完成 |
| `decrypt` | 执行前固定目标卷清单并在 TUI / Web 确认解除保护影响；锁定卷（`0x80310000`）或系统卷残留自动解锁密钥（`0x80310029`）原样报错，不自动解锁或删保护器。调用 `Decrypt()` 启动后台转换后立即返回，仅当 `GetConversionStatus()` 返回 `FullyDecrypted(0)` 时才确认完成；结果为 `unknown` 时先查询卷状态，再决定后续操作 |

凭据与互斥边界：

1. **双路径输入与脱敏**：默认使用 TUI / Web 独立隐藏输入框（3 分钟超时），凭据经标准输入传给 PowerShell，不进入聊天、模型上下文、命令行、临时文件、会话记录、日志或遥测。AI 填参路径须在本地确认模型传输边界，并在 `message_end` 落盘前将聊天与工具参数中的恢复密码替换为脱敏标记，HTTP 快照与 SSE 全程使用脱敏副本。
2. **跨进程同卷互斥**：本机按规范化 `volumeId` 建立命名管道锁，覆盖交互确认、提交与状态复查阶段，占用时拒绝并发操作。
