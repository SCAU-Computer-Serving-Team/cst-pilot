# 诊断与受控工具架构

本页说明 `crates/cst-kit` 中安全守卫与 Windows 原生采集模块的实现结构，涵盖共享卷缓存、命令清单存储与 BitLocker 原生执行机制。行为约束见 [SPEC.md](SPEC.md)，接口定义见 [API.md](API.md)，完整目录映射见 [6.cst-pilot.md](../rust/plan/6.cst-pilot.md)。

## 模块组成与挂载管线

`CstPilotExtension` 通过 `pi-runtime` 公开静态扩展接口挂载全部业务工具、技能与数据策略：

```mermaid
flowchart LR
    Kit["Kit / KitHost\n白名单锁定 + SessionDataPolicy"] --> Guard["tools/\nPathGuard + SSRF + fff + Exa"]
    Kit --> Diag["diagnostics/\nWin32 / WMI / PDH / EvtQuery + WzIndex"]
    Kit --> Runbook["runbook/\n版本方案 + 命名管道目录锁 + BOM/CRLF"]
    Kit --> BitLocker["bitlocker/\nZeroizing 密码 + PacketPrivacy WMI + 同卷锁"]

    Diag --> Job["native_job.rs (Kill-on-Close Job Object)"]
    Job --> Bin["WizTree64.exe (32位回退 WizTree.exe) / cst-lhm-host.exe (.NET 10)"]
```

| 子目录 | 核心文件与职责 |
|---|---|
| `src/kit.rs`、`src/tools/` | 锁定 17 工具白名单，加载 `APPEND_SYSTEM.md` 与 8 份技能。`path_guard.rs` 规范化路径并按文件身份拦截凭据及硬链接。`fff.rs` 基于 `ignore` 与 `regex` 实现检索。`ssrf.rs` 与 `web_access.rs` 实现 DNS 私网拦截与会话有界检索缓存 |
| `src/diagnostics/` | `win_native.rs` 与 `storage_tools.rs` 在阻塞线程池内初始化多线程 COM 并绑定分工具时间预算（常规查询 30 秒，`ls` 与 `disk usage` 215 秒，最多 4 个并发阻塞槽位）。`hardware/pdh.rs` 本地化计数器双采样，`wz_index.rs` 维护卷级快照缓存，`result.rs` 执行 50 KiB 模型文本结构化裁剪 |
| `src/runbook/` | `plan_store.rs` 管理 `command-plans/<ID>/v*.json` 版本锁与导出。`sequence_store.rs` 管理当天递增序号与改名替换。`storage.rs` 持有各级目录句柄拒绝重解析点与硬链接 |
| `src/bitlocker/` | `credentials.rs` 实现 `SessionDataPolicy` 与瞬态 `Zeroizing<String>` 凭据表（释放本机 `BSTR` 前逐单元清零）。`wmi_encryptable.rs` 建立 `PacketPrivacy` 加密 DCOM 连接调用 `UnlockWithNumericalPassword` 与 `Decrypt`。`lock.rs` 维护跨进程卷锁，`core.rs` 执行状态复查 |

## Windows 原生采集与隔离边界

1. **有界阻塞与无泄漏槽位**：COM/WMI、Win32 与 Event Log 调用运行在隔离阻塞线程中，逐批检查取消令牌与时间预算。超时返回后未退出的底层系统调用继续持有工作槽位直至自然返回，防止后台线程无限堆积。SMART 单次最多查询 256 块物理盘。事件日志单通道扫描上限为 100,000 条，超限标记 `scanTruncated`。
2. **Job Object 受控子进程（`native_job.rs`）**：调用 `WizTree64.exe`（32 位系统回退 `WizTree.exe`，上限 180 秒）或 `lhm/host/cst-lhm-host.exe`（上限 20 秒）时，统一以无 Shell 挂起方式创建进程，绑定 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 后恢复运行。取消、超时或主进程崩溃时由内核立即终止整个子进程树。
3. **自包含 LHM 隔离采集**：`crates/cst-kit/lhm-host/` 编译为携带 .NET 10 运行时的独立辅助程序，仅接受 `gpu` 或 `sensor` 模式并从上级目录加载 `LibreHardwareMonitorLib.dll`，将硬件探针异常与主服务内存隔离。

## 共享目录大小缓存（`WzIndex`）

`wz_index.rs` 在进程内存中按盘符缓存 WizTree CSV 导出快照，供后续 `ls` 微秒级下钻查询：

| 内部字段 | 存储内容 |
|---|---|
| `identities` | `盘符 -> Win32 卷序列号 + 文件系统 + 根目录创建时间` |
| `snapshots` | `盘符 -> Arc<ScanSnapshot>`，封装 `dirs`（目录聚合字节数）、`files`（仅保留 ≥1 MiB 文件，补充系统独占文件大小查询）及预聚合排行（`top_dirs`、`top_files`、`stale`、`extensions`） |
| `failed` / `pending` | 当前卷导出失败标记 / 同卷并发合并的在途扫描句柄 |

缓存不变量：

1. **换盘即时失效**：每次读写前调用 `GetVolumeInformationW` 与根目录句柄信息核对卷身份。拔插 U 盘或更换移动硬盘导致卷身份变化时，立即清空该盘符缓存与失败标记。
2. **精确快照发布**：CSV 单行上限为 64 KiB，单卷索引内存上限为 128 MiB，Top-N 最小堆固定保留 100 项。扫描在局部表完成并二次复核卷身份后原子发布，不缓存原生目录遍历产生的截断下界数据。

## BitLocker 隐私策略与中断复查

1. **三处脱敏拦截点**：`SessionDataPolicy` 在写入 `ExecutionStateStore` 前、`message_end` 写入 `SessionStore` 前及向同步／异步订阅者分发事件前统一脱敏恢复密码。流式增量置空未完成的数字尾部，界面直接读取脱敏后的完整 `partial` 快照。AI 填参原文仅在当前轮次请求上下文与 3 分钟过期的 `Zeroizing` 瞬态表中一次性绑定 `tool_call_id` 领取。
2. **跨进程锁与实时复查**：同卷命名管道锁 `\\.\pipe\cst-pilot-bitlocker-<sha256>` 全程覆盖本地确认、WMI 调用与最终复查。调用超时、取消或启动检测到 `requires_state_verification` 时，独立查询 `Win32_EncryptableVolume` 当前锁定与转换状态，写入 `cst.bitlocker-state-verification` 条目并保持 `operation.status = "unknown"`。
