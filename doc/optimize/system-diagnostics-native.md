# 系统诊断原生采集与事件日志优化

本文记录阶段 9.3 中 `sys`、`startup`、`driver` 与 `eventlog` 四组诊断工具的底层改造背景、技术决策与实测对照。架构决策见 [ADR-023](../ADR.md#adr-023系统诊断改用原生-win32nt-接口与结构化事件渲染)，总览进度见 [9.3.optimization.md](../rust/plan/9.3.optimization.md)，接口契约见 [API.md](../tools/API.md)。

## 一、背景

改造前，四组系统诊断工具在采集耗时、权限覆盖、双显卡完整性与时间格式四方面存在瓶颈：

1. **WMI 冷启动与串行查询延迟高**：`sys(overview)` 查询 `Win32_PerfFormattedData_PerfOS_Processor` 冷启动耗 `7,689.3 ms`，且代码仅读取一次。`driver(core)` 串行执行 5 路 WMI，总耗时 `3,936.6 ms`，其中 `Win32_PnPSignedDriver` 占 `2,854.0 ms`。`startup` 查询 `Win32_Service` 耗 `675.1 ms`。
2. **普通权限丢失进程与重复句柄开销**：`sys(proc)` 使用 `Toolhelp32` 加两次 `OpenProcess` 采集 CPU 与工作集，普通权限下仅打开 `348 / 524` 个进程，静默丢失 `176` 个系统与受保护进程，且循环内为每个进程分配 `64 KiB` 路径缓冲。`sys(io)` 两次扫描 WMI `Win32_Process` 耗 `1,222.1 ms`，叠加两次物理盘 WMI 耗 `580.7 ms`，实际执行耗时达 `3.0 s`。
3. **双显卡核显缺失与字段冗余**：`sys(gpu)` 通过 WMI 查进程名耗 `723.5 ms`。检测到 `nvidia-smi` 返回数组后直接跳过 `lhm(Gpu)`，双显卡机型丢失核显传感器。`byGpuPct[].engines[]` 输出零负载引擎条目，`lhmGpu.sensors[]` 缺少单位字段。`driver(core)` 的 `drivers[].date` 透传原始 WMI DMTF 串。
4. **事件日志解析开销与时区错位**：`eventlog` 逐条渲染 XML 再走 `quick_xml` 解析，每条额外分配 `16 KiB` 缓冲。`events[].time` 输出 UTC 时间（末尾 `Z`），`sys(overview).uptime.bootTime` 输出本机时间，两处对齐存在 8 小时偏差。`eventlog(detail)` 超过 `20,000` 字符静默截断，全通道失败仍返回 `Ok` 加空数组。

## 二、决策

保留固有物理采样窗口，针对实测超过 `500 ms` 的采集链路、权限盲区与字段偏差逐项切换原生接口。

### 2.1 核心采纳方案

1. **`sys(proc)` 与 `sys(io)` 共享 `NtQuerySystemInformation` 单次快照**：
   - 在 `crates/cst-kit/src/diagnostics/win_native/` 封装 `NtQuerySystemInformation(SystemProcessInformation = 5)`。单次系统调用获取全量进程的 `UniqueProcessId`、`ImageName`、`CreateTime`、`KernelTime`、`UserTime`、`WorkingSetSize`、`ReadTransferCount` 与 `WriteTransferCount`。
   - 两次采样比对 `(pid, create_time)` 防止 PID 复用串值。仅对最终 Top-N 进程复用单一路径缓冲并复核创建时间后调用 `QueryFullProcessImageNameW`。接口不可用时 `sys(proc)` 回退 `Toolhelp32`，`sys(io)` 回退 WMI。
   - `sys(io)` 物理盘吞吐迁入 PDH `PhysicalDisk`，与进程双采样共用同一 `1.2 s` 窗口。
2. **`sys(overview)` 原生 Win32 替代 WMI 性能计数器**：
   - 内存、内核池与开机时长改走 `GlobalMemoryStatusEx`、`GetPerformanceInfo`、`GetTickCount64` 与 `EnumPageFilesW`。
   - CPU 总占用改用 `GetSystemTimes` 约 `200 ms` 双采样，消除首次加载 WMI 性能计数器的长阻塞。机型与 BIOS 优先读注册表，失败回退 WMI。
3. **`sys(gpu)` 与 `sys(sensor)` 并发优化与字段补齐**：
   - GPU 进程名映射改用 `CreateToolhelp32Snapshot`，替代 WMI `Win32_Process`。
   - 先快速读取显示适配器列表。若存在非 NVIDIA 物理显示适配器（或适配器列表查询失败），在 PDH `1.2 s` 窗口内并行启动 `lhm(Gpu)`，同时保留独显 `nvidia-smi` 与核显传感器。
   - 进程内缓存 PDH 本地化计数器路径，供 `sys(gpu)` 与 `sys(sensor)` 共用。`byGpuPct[].engines[]` 过滤 `gpuPct == 0.0` 项，`lhmGpu.sensors[]` 补齐 `unit` 字段。`lhmGpu` 采集失败写入 `error` 字段，不复用 `notice`。
4. **`startup` 与 `driver(core)` 改用 SCM 与 SetupAPI**：
   - `startup` 自启服务枚举改用 Win32 SCM（`EnumServicesStatusExW` 与 `QueryServiceConfigW`），普通权限零失败，摆脱 WMI 依赖。
   - `driver(core)` 已签名驱动改用 SetupAPI（`SetupDiGetClassDevsW` 与设备属性查询），两项核心服务改用 SCM。`drivers[].date` 由 SetupAPI `DEVPKEY_Device_DriverDate`（`FILETIME`）与 WMI DMTF 统一格式化为 `YYYY-MM-DD`。
5. **`eventlog` 值渲染、时区统一与失败态收口**：
   - 扫描批量改为 `EvtNext(64)`，元数据渲染改为 `EvtRender(EvtRenderEventValues)` 并复用渲染与消息缓冲，免去逐条 `16 KiB` 分配与 XML 解析。
   - `query` 中 `provider` 统一在本地按忽略大小写正则匹配，事件排序与起止时间按原始 64 位 `FILETIME` 比较。
   - `events[].time` 统一改为本机带偏移 RFC3339（如 `2026-11-26T00:35:00+08:00`）。`detail` 超过 `20,000` 字符时补齐 `msgTruncated: true` 与 `originalChars`。请求通道全部失败时返回 `isError: true`。

### 2.2 明确放弃的方案

- **仅用 `OpenProcess + GetProcessIoCounters` 替代 `sys(io)` WMI**：本机普通权限实测仅打开 `348 / 524` 个进程，丢失 `33.6%` 的系统与受保护进程（含 `System`、`MsMpEng`、`TiWorker`）。提权后对受保护进程仍会被内核拒绝。
- **压缩 `sys(proc/io/gpu/sensor)` 的 `1.2 s` 双采样窗口**：缩短采样窗会放大瞬时抖动误差，保持 `1.2 s` 物理观测窗口。
- **`eventlog` 全文关键词下推 XPath**：Windows 事件消息文本由本地消息模板格式化生成，XPath 无法匹配正文，保持 `100,000` 条扫描上限与 `30 s` 超时控制。
- **强制重写 `driver(problem/external/find)`**：现行耗时 `277–434 ms`，本轮聚焦 `driver(core)` 主瓶颈，其余子项按需复用。

## 三、实测数据与结论

### 3.1 测试环境

- **测试日期**：基线与原生接口实测、生产 Release 端到端实测均为 `2026-11-26`。
- **主机配置**：Windows 11 x64，16 逻辑核，普通权限（`Admin=False`），NVIDIA GeForce RTX 5070 Ti 加 GameViewer 虚拟显示器。
- **现场规模**：当前运行 `549–588` 个进程，`System` 通道 `12,443–12,488` 条（`20 MiB`），`Application` 通道 `31,488` 条（`20 MiB`）。

### 3.2 优化前基线与生产 Release 实测对照

| 诊断能力 | 优化前实现与实测 | 原生原型实测 | 生产 Release 实测（`live_windows_tools`） | 关键变化 |
|---|---:|---:|---:|---|
| `sys(overview)` 全量 | WMI 7 路：冷 `7,689.3 ms`+ / 热 `~600 ms` | `GetSystemTimes` `200 ms` + Win32 `2.48 ms` | **`201–203 ms`**（`degraded: None`） | 消除冷启动近 8 秒阻塞，稳定压至 ~200 ms |
| `sys(proc)` 快照与双采 | `Toolhelp32` + 2×`OpenProcess`：`26.0–28.3 ms`（`348/524` 进程） | `NtQuerySystemInformation(5)`：`8.67–12.83 ms`（`527/527`） | 单次 **`17 ms`**（`588` 进程）；端到端 **`1,235–1,237 ms`**（`intervalSec=1.21`） | 覆盖率 `66.4% -> 100%`，双采附加开销约 `25 ms` |
| `sys(io)` 进程读写 + 物理盘 | WMI `Win32_Process` ×2（`1,222.1 ms`）+ WMI `PhysicalDisk` ×2（`580.7 ms`），端到端 `3.0 s+` | `NtQuerySystemInformation(5)` ×2 + PDH 同窗 | 首次 **`1,715–1,867 ms`**（含 PDH 路径本地化）；热态 **`1,230 ms`**（`intervalSec=1.21`） | 进程与物理盘重叠在 `1.2 s` 窗口，热态近零附加耗时 |
| `sys(gpu)` 全量 | WMI 查进程名 `723.5 ms` + PDH，端到端 `~2.0 s`，健康态误报 `degraded` | `Toolhelp32` `17.88 ms` + PDH 路径缓存 | **`1,270–1,275 ms`**（`engineSamples=783`，`counterErrors=null`，`degraded=None`） | 省去 `~700 ms` WMI，零假降级，双卡并行留核显 LHM |
| `sys(sensor)` 全量 | PDH + LHM，无错误时输出空 `counterErrors: {}` | PDH 路径缓存 + 传感器单位 | **`1,201 ms`**（`thermalZones=1`，`freqCores=16`，`counterErrors=null`） | 正常态省略空 `counterErrors`，LHM 传感器补齐 `unit` |
| `startup` 全量 | WMI `Win32_Service`：`675.1 ms`（101 项） | Win32 SCM：`78.81 ms`（102 项，0 失败） | **`64–98 ms`**（`services=101`，`regItems=22`，`degraded=None`） | 提速 `6.9–10.5×`，普通权限零失败 |
| `driver(core)` 全量 | 5 路串行 WMI：`3,936.6 ms`（`PnPSignedDriver` 占 `2,854.0 ms`） | SetupAPI + SCM | **`927 ms`**（`drivers=42`，`bthserv/Audiosrv=Running`，日期 `2026-02-28`） | 提速 `4.2×`，日期统一格式化为 `YYYY-MM-DD` |
| `eventlog`（`System` 720h） | `EvtNext(16)` + XML：`409.6–419.1 ms`（不含 `quick_xml`） | `EvtNext(64)` + Values：`292.4–317.2 ms` | **`307 ms`**（`12,443` 条，时间 `2026-11-26T11:19:31+08:00`） | 减少 `25–30%` 耗时与堆分配，统一本机偏移 RFC3339 |

### 3.3 结论与验收边界

1. **性能与权限覆盖**：生产 Release 实测确认，`sys(overview)` 稳定在 `201–203 ms`，`sys(proc)` 与热态 `sys(io)` 稳定在 `1.23 s`（100% 进程覆盖），`startup` 降至 `64–98 ms`，`driver(core)` 降至 `927 ms`，`eventlog(System 720h)` 降至 `307 ms`。
2. **字段与时区一致**：实测 `events[].time` 已输出 `2026-11-26T11:19:31+08:00`，与 `sys(overview).uptime.bootTime` 可直接对照。`driver(core)` 日期已格式化为 `YYYY-MM-DD`，正常态 `sys(gpu/sensor)` 不再输出空 `counterErrors`。
3. **待补验证边界**：本机无物理双显卡（仅独显加虚拟显示器）且默认非提权，后续发布前需在物理核显加独显机型及管理员权限下复核双显卡并发 LHM 采集。
