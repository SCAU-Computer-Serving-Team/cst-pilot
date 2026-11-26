---
name: sys
description: sys 实时系统状态参考。排查整机 CPU 与内存（overview）、进程排行（proc）、显卡负载与显存（gpu）、磁盘与进程读写（io）或温度风扇频率（sensor）时查阅。
---

# sys

只读实时系统检查工具，普通权限即可调用，权限限制与缺失项随结果说明。

## 排查路径

1. 卡顿、高占用或开机异常先调 `overview`，确定瓶颈落在 CPU、内存、页面文件或内核池。
2. CPU 或内存高转 `proc` 定位进程；`path` 可与 `startup` 交叉核验自启来源。
3. 画面掉帧、显存满或双显卡异常转 `gpu`；磁盘满载转 `io` 并联动 `disk(health)` 与 `eventlog(disk)`。
4. 机身发烫、风扇异常或疑似降频转 `sensor`，必须结合 CPU 负载判读频率低值。
5. 核对顶层 `elapsedMs` 及对应 `[scope]` 内的 `degraded`、`collectionErrors`、`counterErrors` 与 `notice`，缺失字段不推断为正常。

## 参数

| 参数 | 约束与默认值 |
|---|---|
| `scope` | 可选：`overview`（默认）、`proc`、`gpu`、`io`、`sensor` |
| `top` | 可选，排行条数，默认 `10`，上限 `50`，仅作用于 `proc`、`gpu`、`io` |

## 各 scope 返回字段与判读

返回结构为 `{ "<scope>": { ... }, "elapsedMs": N }`。

### overview（整机概况）

- 字段：`cpuTotalPct`（200 ms 短窗双采均值）、`logicalCores`、`mem`（`totalMB`、`freeMB`、`usedMB`、`usedPct`，单位 MiB）、`pagefile[]`（`name`、`allocMB`、`usedMB`、`peakMB`）、`pool`（`nonpagedMB`、`pagedMB`，单位 MiB）、`machine`（`vendor`、`model`、`cpu`、`physicalCores`、`bios`、`biosDate`）、`uptime`（`bootTime` 本地时区 `YYYY-MM-DD HH:MM`、`text`、`totalHours`）。
- 判读：进程工作集无法解释的高内存占用可查 `pool.nonpagedMB`，持续增长提示驱动泄漏线索，单次高值不能单独定性。`machine` 用于核对厂商驱动与 BIOS 版本。

### proc（进程排行）

- 字段：`cores`、`totalProcs`、`intervalSec`（约 1.2 秒双采窗口）、`byCpu[]`、`byMem[]`（每项含 `name`、`pid`、`wsMB`、`cpuPct`、`path`）。
- 判读：`wsMB` 为工作集（MiB）。`cpuPct` 为窗口均值，全核满载为 100%，瞬时突发会被低估。窗口内新建、退出或 PID 复用的进程不参与差分。系统或受保护进程无权限时 `path` 为 `null`。

### gpu（显卡与显存）

- 字段：`engineSamples`、`byGpuPct[]`（`pid`、`name`、`gpuPct`、`engtypes`、`engines[]`）、`byDedicatedMB[]`（`pid`、`name`、`dedicatedMB`）、`adapters[]`（`name`、`vendor`、`driver`、`status`、`bus`）、`nvidia`、`lhmGpu?`、`counterErrors?`。
- 判读：
  1. `gpuPct` 取该进程跨适配器最繁忙单一引擎利用率，不累加并行引擎；`engines[]` 仅保留大于 0 的引擎样本。
  2. `byDedicatedMB` 仅统计大于 1 MiB 的专用显存实例。
  3. `nvidia` 为各 NVIDIA 显卡数组（含 `name`、`uuid`、`pciBusId`、`tempC`、`powerW`、`vramUsedMB`、`vramTotalMB`、`utilPct`、`driver`）；无 `nvidia-smi` 为 `null`，调用失败为 `{ error }`。
  4. 存在非 NVIDIA 物理显卡或 `nvidia` 缺失时并行采集 `lhmGpu`（双显卡机型同时输出独显与核显传感器，统一附 `unit`）。`lhmGpu.hardware` 为空仅说明未枚举到可读传感器，不代表没有显卡。
  5. `adapters[].bus` 为总线前缀，USB 也可能是外接显示设备。

### io（物理盘与进程读写）

- 字段：`intervalSec`（约 1.2 秒窗口）、`totalProcs`、`disks[]`（`disk`、`queueLen`、`busyPct`、`readKBs`、`writeKBs`，单位 KiB/s，按 `busyPct` 降序，不含 `_Total`）、`byIo[]`（`name`、`pid`、`ioKBs`，单位 KiB/s，读写总速率）。
- 判读：持续高 `busyPct` 与高 `queueLen` 提示 IO 压力。低吞吐不能单独证明坏盘，须结合 `disk(health)` 与 `eventlog(disk)`。物理盘吞吐与进程 IO 统计口径不同，不可直接相加；采集无误时 `byIo` 为空表示窗口内无进程读写活动。

### sensor（温度、风扇、电压与频率）

- 字段：`admin`、`pawnio`、`hardware[]`、`sensorCount`、`sensors[]`（`hw`、`name`、`type`、`unit`、`value`，覆盖 Temperature °C、Fan RPM、Voltage V，上限 200 项）、`thermalZones[]`（`zone`、`tempC` -50～150°C、`passivePct`）、`frequency`（`cores`、`minPctOfMax`、`avgPctOfMax`）、`counterErrors?`。
- 判读：`passivePct < 100` 表示该热区处于被动降热；`frequency` 保留超过 100% 的睿频值。低频率既可能是过热降频也可能是省电空闲，须结合 CPU 负载判读。工具不安装内核驱动，不保证 CPU 核心温度、主板风扇与电压一定可读；出现 `counterErrors` 时对应空结果表示采集失败。

## 耗时与错误判读

1. 超时上限 30 秒。`overview` 通常 1 秒内返回，`proc`、`io`、`gpu` 包含约 1.2 秒采样窗口；`elapsedMs` 远高于常态说明系统卡顿、性能计数器响应慢或硬件采集阻塞。
2. 所有采集源全部失败返回 `isError: true`（`{"error", "elapsedMs"}`）；部分采集失败置 `degraded: true` 并附 `collectionErrors` 或 `counterErrors`，禁止将空数组或 `null` 当作正常状态。
