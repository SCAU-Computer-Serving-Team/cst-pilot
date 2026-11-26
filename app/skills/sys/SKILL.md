---
name: sys
description: sys 实时系统状态：整机负载（overview）、进程排行（proc）、显卡与显存（gpu）、磁盘读写（io）、传感器与频率（sensor）。
---

# sys

只读实时系统检查工具，普通权限可调用。返回结构为 `{ "<scope>": { ... }, "elapsedMs": N }`。

## 场景选择

| 现象 | 调用 | 结束条件 |
|---|---|---|
| 卡顿、内存高或需看机型与开机时间 | `sys({ scope: "overview" })` | 列出有读数支持的异常项，或标明尚未定位 |
| CPU 或内存占用高 | `sys({ scope: "proc", top? })` | 列出前 N 进程与路径，可与 `startup` 交叉核验 |
| 画面掉帧、显存满或显卡异常 | `sys({ scope: "gpu", top? })` | 列出引擎负载、显存与各卡状态，核对计数错误 |
| 磁盘读写高或卡顿伴随 IO | `sys({ scope: "io", top? })` | 列出各物理盘与进程 IO，必要时联查 `disk(health)` 与 `eventlog(disk)` |
| 发烫、风扇异常或疑似降频 | `sys({ scope: "sensor" })` | 结合 CPU 负载判读温度、热区与频率，标明缺失项 |

## 参数

| 参数 | 约束与默认值 |
|---|---|
| `scope` | 可选：`overview`（默认）、`proc`、`gpu`、`io`、`sensor` |
| `top` | 可选，默认 `10`，上限 `50`；作用于 `proc`（`byCpu`/`byMem`）、`gpu`（`byGpuPct`/`byDedicatedMB`）与 `io`（仅截断 `byIo`，不截断 `disks`） |

## 各 scope 返回字段与口径

### overview

| 字段 | 含义与口径 |
|---|---|
| `cpuTotalPct` / `logicalCores` | 原生采集为 200 ms 短窗双采均值，回退路径为格式化计数器当前值 / 逻辑核数 |
| `mem` | `totalMB`、`freeMB`、`usedMB`、`usedPct`（内存单位 MiB） |
| `pagefile[]` | `name`、`allocMB`、`usedMB`、`peakMB` |
| `pool` | `nonpagedMB`、`pagedMB`（单位 MiB）。进程工作集无法解释的内存占用可查 `nonpagedMB`，单次高值不能单独定性驱动泄漏 |
| `machine` | `vendor`、`model`、`cpu`、`physicalCores`、`bios`、`biosDate` |
| `uptime` | `bootTime`（本机时区 `YYYY-MM-DD HH:MM`）、`text`、`totalHours` |

### proc

| 字段 | 含义与口径 |
|---|---|
| `cores` / `totalProcs` / `intervalSec` | 逻辑核数 / 进入本次双采差分的进程数 / 约 1.2 秒实际采样间隔 |
| `byCpu[]` / `byMem[]` | `name`、`pid`、`wsMB`（工作集 MiB）、`cpuPct`（窗口均值，全核满载为 100%，瞬时突发会被低估）、`path`（无权限时为 `null`）。窗口内新建、退出或 PID 复用进程不参与差分 |

### gpu

| 字段 | 含义与口径 |
|---|---|
| `engineSamples` | 聚合前的输入引擎样本总数，不是正负载进程数 |
| `byGpuPct[]` | `pid`、`name`、`gpuPct`（取跨适配器最繁忙单一引擎，不累加并行引擎）、`engtypes`、`engines[]: { adapter, type, gpuPct }`（仅保留大于 0 的引擎样本） |
| `byDedicatedMB[]` | `pid`、`name`、`dedicatedMB`（仅聚合大于 1 MiB 的专用显存实例） |
| `adapters[]` | `name`、`vendor`、`driver`、`status`、`bus`（总线前缀，USB 也可能是外接显示设备） |
| `nvidia` | 各 NVIDIA 显卡数组（`name`、`uuid`、`pciBusId`、`tempC`、`powerW`、`vramUsedMB`、`vramTotalMB`、`utilPct`、`driver`）；无 `nvidia-smi` 为 `null`，调用失败为 `{ error }` |
| `lhmGpu?` | 适配器查询失败或检测到非 NVIDIA 物理显卡时提前并行采集，否则仅在 `nvidia` 不是数组时事后补查。含 `hardware`、`sensors[]: { hw, name, type, unit, value }` 及可选 `error`、`degraded`、`collectionErrors`。不保证双显卡都有读数，`hardware` 为空仅表示未枚举到可读传感器 |

### io

| 字段 | 含义与口径 |
|---|---|
| `intervalSec` / `totalProcs` | 约 1.2 秒采样间隔 / 第二次快照进程总数 |
| `disks[]` | `disk`、`queueLen`（队列深度）、`busyPct`（%）、`readKBs`、`writeKBs`（读写吞吐单位 KiB/s，按 `busyPct` 降序，不含 `_Total`） |
| `byIo[]` | `name`、`pid`、`ioKBs`（读写总速率，单位 KiB/s）。物理盘吞吐与进程 IO 统计口径不同，不可相加。`byIo` 为空表示本次未取得可比较的非零读写差分（新建、退出、无法配对或计数缺失的进程不入榜） |

### sensor

| 字段 | 含义与口径 |
|---|---|
| `admin` / `pawnio` | 当前进程管理员状态 / PawnIO 注册项探测结果；探测失败时均可为 `null` |
| `hardware[]` / `sensorCount` / `sensors[]` | 硬件名 / 传感器数量 / `hw`、`name`、`type`、`unit`、`value`（Temperature °C、Fan RPM、Voltage V，上限 200 项） |
| `thermalZones[]` | `zone`、`tempC`（-50～150°C）、`passivePct`（`% Passive Limit` 读数，不能单独作为正在过热或被动降热的判据） |
| `frequency` | `cores`（有效频率样本数）、`minPctOfMax`、`avgPctOfMax`（睿频可超 100%）。低频率既可能来自省电空闲也可能来自限频，须结合 CPU 负载判读 |

工具不安装内核驱动，不保证 CPU 核心温度、主板风扇与电压一定可读。

## 错误与超时规则

1. 超时上限 30 秒（`proc`、`gpu`、`io` 含约 1.2 秒采样窗口）。超时或全部采集源失败返回 `isError: true`（`{"error", "elapsedMs"}`）。
2. 部分采集失败在对应 `[scope]` 内置 `degraded: true`，并附 `collectionErrors` 或 `counterErrors`；缺失字段或空数组须结合错误项判读，不可推断为正常。
