# 诊断工具架构

本页说明 Rust 诊断模块与 TypeScript 扩展的模块组织、系统采集边界、共享目录大小缓存（`wz-index`）及跨进程互斥实现。行为规则见 [SPEC.md](SPEC.md)，接口定义见 [API.md](API.md)。

## Rust 原生诊断

`crates/cst-pilot/src/diagnostics/` 通过 `CstPilotExtension` 挂载。当前提供 `ls`、`disk` 与 `sys` 全部子功能，以及 `driver`、`startup` 与 `eventlog`。业务迁移顺序见 [阶段 6 计划](../rust/plan/6.cst-pilot.md)。

| 模块 | 职责 |
|---|---|
| `win_native.rs` 与 `win_native/windows/` | 定义可替换的只读采集接口；实现原生 COM/WMI、进程、卷空间、注册表与 Event Log 访问 |
| `mod.rs` | 注册工具，常规查询绑定取消与 30 秒预算；最多 4 个阻塞采集任务，超时后仍由运行中的任务持有名额，直到系统调用退出 |
| `storage_tools.rs`、`ls.rs`、`scan.rs`、`walk.rs` | 注入共享缓存，浏览目录，流式解析 CSV 并维护 Top-N；WizTree 最多 180 秒，降级遍历共用 50 万条目 / 30 秒预算 |
| `wz_index.rs`、`volume.rs` | Win32 卷序列号、文件系统与根目录创建时间核验，同卷扫描合并，完整结果发布与子树更新；失败或换盘时不复用过期数据 |
| `native_job.rs` | 无 Shell 子进程挂起创建、显式句柄继承、绑定 kill-on-close Job Object 后运行；输出限长，取消或超时后终止整个进程组 |
| `sys.rs`、`sys_load.rs`、`disk.rs`、`driver.rs`、`startup.rs`、`eventlog.rs` | 参数校验、双采样差分、单位换算、筛选聚合与部分失败标记 |
| `hardware.rs`、`hardware/pdh.rs` | 可替换的硬件采集接口、PDH 计数器本地化、NVIDIA 命令与隔离 LHM 辅助程序调用 |
| `result.rs` | 模型文本按数组和字符串递减裁剪至 50 KiB，界面 `details` 保留完整对象 |

COM 在采集线程内初始化并配对释放。WMI 使用异步查询流，取消或超时后撤销查询；Win32 与 Event Log 枚举逐批检查预算。SMART 逐盘查询并独立保留错误，单次最多查询 256 个物理盘；事件日志单通道最多扫描 100,000 条，达到上限时标记 `scanTruncated`，统计值仅覆盖已扫描范围。

CSV 单行限制 64 KiB，卷索引按 128 MiB 预算限制；Top-N 最小堆固定保留 100 项。扫描临时目录逐级拒绝链接并持有目录句柄，CSV 读取器释放时删除文件。遍历结果仅用于当前查询，不写入共享缓存；任一统计不完整时全部占比置空。

PDH 通过 `PdhAddEnglishCounterW` 与 `PdhGetCounterInfoW` 获取本地化通配路径，再进行双采样和实例数组读取；缓冲区最多 8 MiB。GPU 引擎失败重试一次，每进程取跨适配器最大引擎值。I/O 按 PID 与创建时间核对后差分，物理盘格式化计数器首读只预热。

LHM 通过 `crates/cst-pilot/lhm-host/` 的自包含 .NET 10 辅助程序读取，固定路径为 `lhm/host/cst-lhm-host.exe`，DLL 与依赖位于上一级。程序仅接收 `gpu` 或 `sensor`，绑定 Job Object，最多 20 秒；读数过滤非有限值并限制 200 项，关闭失败与部分更新错误保留在结果中。NVIDIA 命令使用系统目录固定路径，最多 8 秒。热区、频率与 LHM 独立保留可用数据，权限和 PawnIO 注册项不用于推断传感器可用性。

自动测试注入采集器替身，并验证真实事件 XML 的解析格式和模拟 LHM DLL 的隔离进程调用。Windows 数据源实机验收沿用 [TESTING.md](../TESTING.md#诊断工具公共核查组b01b35)。

## Rust 命令方案与交付

`crates/cst-pilot/src/runbook/` 通过同一个 `CstPilotExtension` 注册 `command_plan`、`runbook_deliver` 与 `runbook`。方案 JSON 和 TXT 格式与现有文件保持一致，`outbox/.plans` 仅用于读取存量方案。

| 模块 | 职责 |
|---|---|
| `model.rs` | 参数、风险档、单行条目、归档日期与 BOM + CRLF 文本格式 |
| `plan_store.rs` | 读取最新版本、冲突校验、递增保存与指定版本导出；同版本内容不变时复用原文件 |
| `sequence_store.rs` | 当天序号分配与重复检测、同序号改名替换、Windows 命名管道目录锁 |
| `storage.rs` | 逐级拒绝链接与重解析点、核对打开文件身份并拒绝硬链接、同目录临时文件同步与原子替换 |
| `mod.rs` | 工具注册与进程级异步写入互斥；阻塞文件访问在工作线程执行，取消后在提交前停止 |

Windows 下持有各级目录句柄并禁止删除共享，覆盖检查、创建和文件提交期间。方案与导出共用 `command-plans` 锁，单步交付使用当天目录锁；管道名与 TypeScript 写入器一致。修改序号文件时先保存新文件，删除原文件名失败则回收新文件，保留原清单。

自动测试使用隔离目录、真实文件与命名管道，覆盖文件占用、junction、硬链接及进程退出后的锁释放；另以模拟模型驱动真实 SDK 验证连续两轮方案修订与交付。

## Rust BitLocker 凭据边界

`crates/cst-pilot/src/bitlocker/credentials.rs` 由 `CstPilotExtension` 注册消息与上下文钩子。`Kit` 固定注入 `SessionDataPolicy`，覆盖持久输入副本、最终消息和订阅事件。工具定义与消息钩子共用同一个凭据表，`bitlocker` 固定顺序执行且禁止自动重放。

瞬态用户副本和工具凭据使用 `Zeroizing<String>`，最多保留 3 分钟。工具凭据绑定调用 ID、规范化盘符与 `unlock`，领取后立即从表中移除；轮次结束、会话启动与关闭时清理。用户原文仅恢复至本轮模型请求副本，持久记录继续使用脱敏文本。

订阅投影同时处理消息、工具参数、结果和错误。流式增量置空，未完成的数字尾部隐藏，界面读取脱敏后的完整 `partial`；同步和异步订阅器使用同一份投影结果。底层 Agent 与存储接口供可信宿主使用，后续 HTTP 快照需应用相同业务脱敏规则。

### 原生执行与复查

| 模块 | 职责 |
|---|---|
| `bitlocker/model.rs` | 参数、卷状态与操作结果，规范化盘符和卷身份 |
| `bitlocker/core.rs` | 固定目标、本地交互、执行前复核与独立状态复查 |
| `bitlocker/wmi_encryptable.rs` | 本机 COM/WMI 连接，对服务、枚举与结果代理设置 PacketPrivacy；仅调用状态方法、解锁与解密 |
| `bitlocker/lock.rs` | 按规范化卷身份建立与 TypeScript 兼容的命名管道锁 |

本地隐藏输入和确认通过 `ExtensionUiContext` 传递，等待上限 180 秒。原生调用最多占用 4 个工作槽位，每次等待预算 60 秒；超时或取消后立即独立查询，旧锁定与转换字段不回填。工作线程在原方法结束后继续复查；调用方被丢弃时也执行该复查，槽位与卷锁随工作线程释放。

卷锁覆盖输入、提交和复查。变更前重新枚举并核验卷身份、盘符、管理员权限与当前状态，保留 Windows 原始返回码；未知结果保持 `unknown`，全程不重放。`Kit::create_session` 在启动报告要求核验 BitLocker 时查询真实状态，返回 `KitSession::bitlocker_verification` 并保存 `cst.bitlocker-state-verification` 条目。

Rust 密码副本使用 `Zeroizing<String>`，本机 BSTR 释放前覆写，输入对象提交后清空密码属性；COM 编组副本由 Windows 管理。自动测试仅使用模拟卷、模拟方法和通用 UI 通道；真实加密卷单独验收。

## TypeScript 扩展结构

`agent/home/extensions/diagnostics/index.ts` 是唯一扩展入口，每次加载时创建一份共享目录大小索引 `createVolumeIndex()` 并注入 `disk` 与 `ls`，统一注册 10 个工具。

| 模块文件 | 职责 |
|---|---|
| `index.ts` | 扩展入口，创建 `wz-index` 实例并注册 10 个工具 |
| `ls.ts`、`startup.ts` | 独立查询工具的参数校验、执行与返回封装 |
| `runbook.ts` / `runbook-files.ts` | 命令工具注册、共用参数校验、文本格式、单步写入及文件锁 |
| `command-plan.ts` / `command-plan-store.ts` | 方案／交付工具注册与版本存储；`agent/home/command-plans/<ID>/v<版本>.json` 为方案事实来源，导出文本保持版本内容 |
| `disk.ts` / `disk-core.ts` | 存储工具注册与空间、物理盘、SMART、WizTree／Node 扫描实现 |
| `sys.ts` / `sys-commands.ts` | 实时负载与传感器注册、采集编排及 PowerShell 采样脚本模板 |
| `driver.ts` / `driver-core.ts` / `driver-data.ts` | 设备与驱动工具注册、WMI 查询、Node 侧过滤与结果 Schema 校验 |
| `eventlog.ts` / `eventlog-core.ts` | 事件日志注册、分组 `FilterHashtable` 下推、后置过滤、去重与异常分类 |
| `bitlocker.ts` / `bitlocker-*.ts` | BitLocker 注册、提示词与脱敏、逐卷路由、CIM 标准输入传参、TUI/Web 交互桥与跨进程卷锁 |
| `wz-index.ts` | `disk usage` 与 `ls` 共用的进程内卷级目录与大文件大小缓存 |
| `runtime.ts` | 随包 `pwsh` / `wiztree` / `lhm` 路径解析、子进程管理、取消信号传递与 UTF-8/GBK 解码 |
| `pwsh-data.ts` | 动态字符串安全编码（防脚本注入）与 PowerShell 非终止错误收集包装 |
| `result.ts` | 模型文本 50 KiB 限长裁剪、`details` 保留与整次错误抛出 |

## PowerShell 调用与安全边界

```mermaid
flowchart LR
    A[工具输入参数] --> B[TypeBox 与白名单校验]
    B --> C[pwsh-data.ts 编码为安全数据表达式]
    C --> D[随包 pwsh.exe -NoProfile -NonInteractive]
    D --> E[严格 UTF-8 解码 / 失败回退 GBK]
    E --> F[解析 JSON + 提取非终止错误]
    F --> G[result.ts 50KiB 裁剪文本 + 完整 details]
```

1. **零注入参数传递**：动态字符串一律通过 `pwsh-data.ts` 编码为 PowerShell 安全数据表达式，禁止将未转义引号或用户输入直接拼接到脚本语法中；BitLocker 恢复密码通过子进程 `stdin` 传递，不出现在命令行参数中。
2. **输出解码与错误分类**：`runPwsh()` 对 `stdout` / `stderr` 优先按严格 UTF-8 解码，遇到中文系统本地化报错（ANSI/GBK 代码页）解码失败时自动回退 GBK。`sys`、`driver`、`startup` 捕获 `$Error` 中的非终止错误转为 `collectionErrors` 与 `degraded`；`eventlog` 按异常类型与 `FullyQualifiedErrorId` 区分权限拒绝、通道缺失与单条记录损坏，不依赖本地化错误文本。
3. **取消与超时**：工具 `execute` 接收的 `AbortSignal` 逐层传入子进程与目录遍历，取消或超时后立即终止子进程并清理临时文件。

## 共享目录大小缓存（wz-index）

`wz-index.ts` 在进程内存中复用 WizTree CSV 导出结果，加速同卷后续 `ls` 查询，不向模型暴露为独立工具。

| 内部结构 | 存储内容 |
|---|---|
| `dirs` | `盘符 -> 规范化大写目录路径 -> 聚合字节数` |
| `files` | `盘符 -> 规范化大写文件路径 -> 字节数`（仅保存 ≥1 MB 的文件，用于 `stat` 失败或系统独占文件大小为 0 时补查） |
| `identities` | `盘符 -> 卷身份标识`（卷设备 ID + 根目录创建时间） |
| `failed` / `pending` | 当前卷建缓存失败标记 / 同卷正在执行的导出任务 Promise |

缓存生命周期与不变量：

1. **卷身份核验**：每次读取或写入缓存前核对目标盘符的卷身份；换盘或身份无法读取时立即清空该盘符的旧缓存与失败标记。
2. **并发合并与原子发布**：同卷并发 `ls` 共享同一个 `pending` 导出任务；扫描在局部 Map 中完成后再次核对卷身份，再整体替换或更新子树，读取方不会读到半成品。
3. **只收精确快照**：仅接收 WizTree 导出数据，不缓存 Node 递归统计结果，防止权限拒绝或预算截断产生的下界污染后续查询。当前卷 WizTree 失败后标记 `failed`，避免后续每次 `ls` 重复等待超时。

## 跨进程互斥锁

- **方案版本锁**：`command_plan` 与 `runbook_deliver` 共用 `agent/home/command-plans` 目录锁。读取当前版本、核对版本和原子发布均在锁内完成；JSON 与 TXT 经临时文件重命名发布，失败后清理临时文件。

- **runbook 日期目录锁**：同一进程内通过串行队列排队；跨进程按 `artifacts/<YYYY-MM-DD>/runbook` 路径哈希建立 Windows 命名管道锁，冲突时直接拒绝写入，防止多进程分配重复序号或覆盖修正文件。
- **bitlocker 同卷锁**：按规范化 `volumeId`（统一大写与尾部反斜线，不含盘符或会话 ID）建立本机命名管道锁 `bitlocker-lock.ts`。取得锁后重新调用 Windows 接口核实卷身份与当前状态，覆盖本地确认、提交与状态复查，进程退出时由操作系统自动释放。
