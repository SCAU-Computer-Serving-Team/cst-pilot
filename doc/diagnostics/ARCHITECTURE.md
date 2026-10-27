# 诊断工具架构

本页说明 `diagnostics` 扩展的模块组织、PowerShell 子进程调用与安全编码、共享目录大小缓存（`wz-index`）及跨进程互斥实现。行为规则见 [SPEC.md](SPEC.md)，接口定义见 [API.md](API.md)。

## 扩展结构

`agent/home/extensions/diagnostics/index.ts` 是唯一扩展入口，每次加载时创建一份共享目录大小索引 `createVolumeIndex()` 并注入 `disk` 与 `ls`，统一注册 10 个工具。

| 模块文件 | 职责 |
|---|---|
| `index.ts` | 扩展入口，创建 `wz-index` 实例并注册 10 个工具 |
| `ls.ts`、`startup.ts` | 独立查询工具的参数校验、执行与返回封装 |
| `runbook.ts` / `runbook-files.ts` | 命令工具注册、共用参数校验、文本格式、单步写入及文件锁 |
| `command-plan.ts` / `command-plan-store.ts` | 方案／交付工具注册与版本存储；`outbox/.plans/<ID>/v<版本>.json` 为方案事实来源，导出文本保持版本内容 |
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

- **方案版本锁**：`command_plan` 与 `runbook_deliver` 共用 `outbox/.plans` 目录锁。读取当前版本、核对版本和原子发布均在锁内完成；JSON 与 TXT 经临时文件重命名发布，失败后清理临时文件。

- **runbook 日期目录锁**：同一进程内通过串行队列排队；跨进程按 `outbox/<YYYY-MM-DD>` 路径哈希建立 Windows 命名管道锁，冲突时直接拒绝写入，防止多进程分配重复序号或覆盖修正文件。
- **bitlocker 同卷锁**：按规范化 `volumeId`（统一大写与尾部反斜线，不含盘符或会话 ID）建立本机命名管道锁 `bitlocker-lock.ts`。取得锁后重新调用 Windows 接口核实卷身份与当前状态，覆盖本地确认、提交与状态复查，进程退出时由操作系统自动释放。
