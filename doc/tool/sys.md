# sys：实时负载与传感器

查看整机、进程、GPU、磁盘 IO 和可读传感器。开机配置使用 [startup](startup.md)，历史故障使用 [eventlog](eventlog.md)。

实现：[sys.ts](../../agent/home/extensions/diagnostics/sys.ts)；设计见 [sys 设计](../design/tool/sys-design.md)。

## 调用

```js
sys({})
sys({ scope: "proc", top: 5 })
sys({ scope: "gpu", top: 5 })
sys({ scope: "io", top: 5 })
sys({ scope: "sensor" })
```

| 参数 | 说明 |
|---|---|
| `scope` | overview（默认）、proc、gpu、io、sensor |
| `top` | proc/gpu/io 的排行项数，默认 10，最大 50 |

结果按 scope 包装，例如 `{ "proc": { ... } }`。读取数值前先看 `notice`、`error`、`collectionErrors` 和 `counterErrors`；部分数据源失败时，其余结果仍可能可用。

## overview：整机概况

| 字段 | 内容 |
|---|---|
| `cpuTotalPct` / `logicalCores` | CPU 总占用与逻辑核数 |
| `mem` | totalMB、usedMB、freeMB、usedPct |
| `pagefile[]` | name、allocMB、usedMB、peakMB |
| `pool` | nonpagedMB、pagedMB：内核内存池 |
| `machine` | vendor、model、cpu、physicalCores、bios、biosDate |
| `uptime` | bootTime、text、totalHours |

CPU 使用格式化计数器类预读后再次采样；其他字段主要来自 CIM 快照。机型信息用于后续核对厂商资料，默认字符串或空值不等于采集失败。内存池持续增长可以作为驱动泄漏线索，单次高值不能确认泄漏。

## proc：进程 CPU 与内存

| 字段 | 内容 |
|---|---|
| `cores`、`totalProcs`、`intervalSec` | 逻辑核数、采集进程数、实际采样间隔 |
| `byCpu[]` / `byMem[]` | 分别按 CPU 与工作集排序的前 N 项 |
| 每项 | name、pid、wsMB、cpuPct、path |

CPU 在同一 PowerShell 进程内双采样，等待约 1.2 秒，用 Stopwatch 的实际间隔计算：

```text
cpuPct = 100 × CPU 时间增量 ÷ 实际间隔 ÷ 逻辑核数
```

`wsMB` 是工作集，不能与任务管理器任意内存列混用。`path` 在权限不足或系统进程上可为 null。采样间新建、退出或无法读取的进程可能不进入排行；窗口平均值也会掩盖短暂峰值。

## gpu：利用率与显存

| 字段 | 内容 |
|---|---|
| `byGpuPct[]` | 每进程 GPU 利用率排行 |
| `byDedicatedMB[]` | 每进程专用显存排行 |
| `engineSamples` | 聚合前引擎样本数 |
| `adapters[]` | name、vendor、driver、status、bus，含虚拟适配器 |
| `nvidia` | 每张 NVIDIA 卡的状态数组；程序不存在为 null，调用失败为 error 对象 |
| `lhmGpu` | nvidia-smi 不存在或失败时尝试的 LHM 补充数据，含 hardware、sensors |

byGpuPct 每项含 pid、name、gpuPct、engtypes（类型组合字符串）和 engines（adapter/type/gpuPct 样本）；byDedicatedMB 每项含 pid、name、dedicatedMB。**gpuPct 取该进程跨适配器最繁忙引擎的利用率，不累加并行引擎。** 专用显存按进程聚合，只计入超过 1 MB 的实例样本。

nvidia 数组每项含 name、uuid、pciBusId、tempC、powerW、vramUsedMB、vramTotalMB、utilPct、driver；不支持的数值返回 null。uuid/pciBusId 用于区分多张卡。

GPU Engine 和 GPU Process Memory 分别读取，失败原因放入 counterErrors；引擎查询失败重试一次。计数器失败不会丢弃已获取的 NVIDIA 数据。路径通过 Windows PDH 转为本地化名称，不要求目标系统保留英文名称。

注意：

- nvidia 为 null 只说明 `System32/nvidia-smi.exe` 不存在，不代表没有显卡。
- lhmGpu.hardware 为空表示没有枚举到可读硬件，不能取代 adapters 判断。
- bus 是设备 ID 的总线前缀。PCI、ROOT、USB 只能作为线索，USB 也可能是实体显示设备。
- 利用率是瞬时值，空闲时为 0 合理；仍需确认计数器没有报错。

## io：磁盘与进程读写

| 字段 | 内容 |
|---|---|
| `intervalSec` / `totalProcs` | 实际窗口与采集进程数 |
| `disks[]` | disk、queueLen、busyPct、readKBs、writeKBs，按忙碌度排序 |
| `byIo[]` | name、pid、ioKBs；窗口内有 IO 活动的进程排行 |

每盘指标来自 `Win32_PerfFormattedData_PerfDisk_PhysicalDisk`，排除 `_Total`；busyPct 按 100 − PercentIdleTime 计算。进程速率来自 `Win32_Process` 的 Read/WriteTransferCount 差分，与磁盘指标共用采样窗口。

进程 IO 计数按进程归属，与磁盘总吞吐是两个口径，不能将 byIo 与某一磁盘吞吐直接相加对账。持续高忙碌度和排队可提示 IO 瓶颈；低吞吐不直接证明碎片或坏盘，应结合访问模式、SMART 和事件日志核查。全空闲时 byIo 为空合法。

## sensor：温度、风扇与降频线索

| 字段 | 内容 |
|---|---|
| `admin` / `pawnio` | 当前权限与 PawnIO 检测结果 |
| `hardware` / `sensorCount` / `sensors[]` | LHM 硬件与读数；每项含 hw、name、type、value |
| `thermalZones[]` | zone、tempC、passivePct |
| `frequency` | cores、minPctOfMax、avgPctOfMax |
| `counterErrors` | 热区或频率读取失败的原因；对应空值不代表设备不存在 |

LHM 随包提供，传感器查询开启 CPU、GPU、主板，关闭存储、内存和嵌入式控制器路径。过滤 NaN/Infinity，最多返回 200 个传感器。热区温度由开尔文转换为摄氏度，过滤超出 −50～150°C 的值；热区名称由 ACPI 提供，不能一律认作 CPU 核心温度。

`passivePct<100` 表示热区被动降热限制；频率百分比低也可能来自省电、功耗限制或调度。**低频率即使伴随高负载，也只是需要继续核查的线索，不能单独确认过热。**

### 能力边界

本项目不安装内核驱动，因此不保证 CPU 核心温度、主板风扇或电压可读。旧 LHM 的 WinRing0 路线有驱动安全与兼容问题；PawnIO 需要宿主安装，均不作为随包安装方案。目标机已有相应驱动和权限时，可能返回更多传感器。

2026-09-01 的提权验证中，admin=true 但未安装 PawnIO 时，CPU 可枚举，Temperature/Clock/Power 仍为空。这证明该机器上仅提权不能解决，不是所有机型的测试结论。

## 性能与判读限制

- overview/io 的 WMI 提供程序首次启动可能明显更慢；移动介质还会增加 pwsh 启动耗时。
- proc/io 使用窗口数据，gpu 是瞬时采样，不要求不同调用的值相等。
- 传感器和性能计数器不是所有系统必备；连续失败需根据错误定位，不能统一解释为损坏。
- sys 不安装驱动、改变电源方案或控制风扇。容量与硬盘可靠性使用 disk，设备枚举异常使用 driver。
