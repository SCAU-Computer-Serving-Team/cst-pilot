---
name: sys
description: sys 工具的参数与返回字段说明。覆盖 scope 枚举含义、参数定义、五个 scope 的数据来源、采样方式、耗时与字段语义、CPU 核心温度不可得的原因。
---

# sys 工具说明

只读系统检查工具，按 `scope` 参数返回运行时状态，不做任何修改。不传 `scope` 时默认 `overview`。五个 scope 均可在普通权限下调用；缺失能力和权限限制通过结果说明。

## 参数

1. `scope`（可选）：`overview` / `proc` / `gpu` / `sensor` / `io`，不传默认 `overview`
2. `top`（可选）：Top N 条数，默认 10，上限 50，仅对 `proc` / `gpu` / `io` 生效

## scope=overview：整机负载快照

1. 数据来源：CPU 总占用率取 `Win32_PerfFormattedData_PerfOS_Processor`（格式化计数器类，读两次取第二次，类名不随系统语言本地化）；内存取 `Win32_OperatingSystem`；页面文件取 `Win32_PageFileUsage`；内核内存池取 `Win32_PerfFormattedData_PerfOS_Memory`；机型取 `Win32_ComputerSystem` / `Win32_BIOS` / `Win32_Processor`；开机时长取 `LastBootUpTime` 与当前时间差
2. 字段：`cpuTotalPct`（近 1 秒差分值）/ `logicalCores` / `mem`（totalMB / usedMB / freeMB / usedPct）/ `pagefile`（allocMB / usedMB / peakMB）/ `pool`（nonpagedMB / pagedMB）/ `machine`（vendor / model / cpu / physicalCores / bios / biosDate）/ `uptime`（bootTime / text / totalHours）
3. 判读：进程工作集无法解释的内存占用，可继续核查 `pool.nonpagedMB`。内核内存池持续增长可作为驱动泄漏线索，单次高值不能确认原因。机型与 BIOS 版本用于核对厂商资料

## scope=proc：进程盘点

1. 数据来源：pwsh 单命令内 1.2 秒双采样差分——两次 `Get-Process` 取 `TotalProcessorTime`，按 Stopwatch 计的实际间隔与逻辑核数折算
2. 字段：`byCpu`（CPU 占用率 Top N）/ `byMem`（内存 Top N）；每项含 `name` / `pid` / `wsMB` / `cpuPct` / `path`
3. 字段语义：`wsMB` 是工作集，需与任务管理器相同口径的列对照；`cpuPct` 是采样窗口内的平均值，瞬时突发会低估；单进程吃满全部逻辑核时显示 100%；`path` 是可执行文件路径，系统进程或权限不足时为 null，可用于就地验证进程身份（与 startup 的自启项交叉核对）

## scope=gpu：GPU 状态

1. 数据来源：`GPU Engine` 与 `GPU Process Memory` 性能计数器按 pid 聚合（通过 Windows PDH 将英文路径转换为本地化路径；偶发无效采样会自动重试 1 次）；适配器清单取 `Win32_VideoController`；检测到 `nvidia-smi`（系统驱动自带）时并行附带显卡状态
2. 字段：`byGpuPct`（每进程 GPU 利用率 Top N，`engtypes` 为该进程用到的引擎类型清单，如 3d / copy / videodecode / videoencode）/ `byDedicatedMB`（每进程专用显存 Top N）/ `adapters`（显卡适配器清单：name / vendor / driver / status / bus）/ `nvidia`（每张 NVIDIA 卡的状态数组，含 uuid / pciBusId / 温度 / 功耗 / 显存 / 利用率 / 驱动版本；程序不存在为 null，调用失败保留 error）/ `engineSamples`（聚合前原始实例数）/ `lhmGpu`（nvidia-smi 不存在或失败时出现，LHM 用户态读的核显 / 其他卡原始传感器）
3. 字段语义：`gpuPct` 是该进程跨适配器最繁忙引擎的瞬时利用率，不累加并行引擎；`engines` 保留适配器和引擎样本；`nvidia: null` 只说明 nvidia-smi 不存在，不代表没有显卡；`adapters.bus` 是设备 ID 的总线前缀，需结合设备名、厂商与机型识别，USB 也可能是实体显示设备；`lhmGpu.hardware` 为空仅表示未枚举到可读硬件，不能代替 adapters 判断。每进程利用率是否可用还需检查计数器错误

## scope=io：磁盘 IO 定位

1. 适用：检查卡顿是否伴随磁盘或进程 IO 活动。一条 pwsh 命令内两路数据共用同一采样窗口（Stopwatch 计真实间隔）：每进程 IO 取 `Win32_Process` 的 `Read/WriteTransferCount` 两次快照差分（进程启动以来累计值，不走 PerfProc 慢路径）；每盘取 `Win32_PerfFormattedData_PerfDisk_PhysicalDisk` 格式化计数器类双读（首读丢弃）
2. 字段：`disks`（每物理盘：`disk` / `queueLen` 队列深度 / `busyPct` 忙碌百分比 / `readKBs` / `writeKBs`，按忙碌度降序）/ `byIo`（每进程读+写 IO 速率 Top N：`name` / `pid` / `ioKBs`）/ `intervalSec` / `totalProcs`
3. 判读：持续高忙碌度与排队可提示 IO 瓶颈。低吞吐不能单独证明碎片或坏盘，需结合访问模式、SMART 与事件日志。进程 IO 和物理盘吞吐口径不同；byIo 空数组在无活动且采集成功时合法。首次调用可能包含计数器预热

## scope=sensor：温度 / 风扇 / 电压 / 降频

1. 数据来源（一条 pwsh 命令内顺序取三路）：LibreHardwareMonitorLib 用户态读取（DLL 随仓库 `lhm\` 分发，GPU 类传感器免管理员）；`Thermal Zone Information` 性能计数器（热区温度，开尔文已转摄氏度）；`Processor Information` 的 `% of Maximum Frequency` 计数器
2. 字段：`sensors`（hw / name / type / value，type 为 Temperature / Fan / Voltage）/ `thermalZones`（zone / tempC / passivePct）/ `frequency`（cores / minPctOfMax / avgPctOfMax）/ `admin` / `pawnio` / `hardware`（检测到的硬件名）/ `counterErrors`（仅计数器读取失败时出现：thermal / frequency 附原因）
3. 字段语义：`passivePct` < 100 表示该热区正在被动降热；`minPctOfMax` 是各核当前频率占最大频率的最低百分比——过热降频与省电降频都表现为低值，该值需结合 CPU 负载解读；`counterErrors` 出现时，对应字段为空是读取失败，不代表机器没有热区 / 降频计数器
4. 能力边界：工具包不安装内核驱动，不保证 CPU 核心温度、主板风扇和电压可读。目标机已有 PawnIO 且权限足够时，可能返回更多传感器。低频率即使伴随高负载，也不能单独确认过热

## 通用约定

1. 结果中的 `notice` 字段是降级/附注说明，转达给队员时不能省略
2. `error` 字段表示该路数据取数失败并附原因，如实转达
