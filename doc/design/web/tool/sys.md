# sys 工具卡片

[sys.ts](../../../../agent/home/extensions/diagnostics/sys.ts) 返回 `details[scope]`，默认 `overview`。2026-09-23 逐 scope 直接调用项目扩展；以下均为实际 `details` 节选，列表只列首项及其真实字段。诊断工具的 `content` 是同一结果的 JSON 文本。

## 主模板与字段组

| scope | 主模板 | 字段组与排行组 |
|---|---|---|
| `overview` | 概览组，醒目样式 | 内存 `mem`、整机 CPU `cpuTotalPct/logicalCores`、页面文件 `pagefile[]`、内核池 `pool`、机型 `machine`、开机时间 `uptime` |
| `proc` | 排行组 | `byCpu[]` 和 `byMem[]` 分列排行；`totalProcs/cores/intervalSec` 作为字段组 |
| `io` | 排行组 | `disks[]` 按忙碌度、`byIo[]` 按进程读写速率分列排行；`intervalSec/totalProcs` 为字段组 |
| `gpu` | 排行组 | `byGpuPct[]/byDedicatedMB[]` 分列排行；`adapters[]/nvidia[]/lhmGpu` 为硬件字段组 |
| `sensor` | 概览组 | `sensors[]` 按硬件、`thermalZones[]` 按热区重复字段组；`frequency/admin/pawnio` 为字段组 |

`notice`、`collectionErrors`、`counterErrors`、`degraded` 随对应 scope 显示。所有数组为空时须保留采集口径；读数 `null` 表示未知，不能显示为健康。

## 各 scope 的实测返回

### overview

```json
{"overview":{"cpuTotalPct":19,"logicalCores":16,"mem":{"totalMB":32557,"usedMB":27214,"freeMB":5343,"usedPct":83.6},"pagefile":[{"name":"C:\\pagefile.sys","allocMB":32983,"usedMB":3218,"peakMB":4580}],"pool":{"nonpagedMB":1851,"pagedMB":1362},"machine":{"vendor":"Maxsun","model":"Default string","cpu":"12th Gen Intel(R) Core(TM) i5-12600KF","physicalCores":10,"bios":"American Megatrends International, LLC. H7.4G","biosDate":"2025-02-23"},"uptime":{"bootTime":"2026-09-20 09:45","text":"3天8小时51分","totalHours":80.9}}}
```

### proc

输入 `top:2`；实测 `totalProcs:351`、`cores:16`、`intervalSec:1.24`，两份排行各返回 2 项：

```json
{"proc":{"byCpu":[{"name":"node","pid":33844,"wsMB":364,"cpuPct":1.5,"path":"C:\\Program Files\\nodejs\\node.exe"}],"byMem":[{"name":"SlayTheSpire2","pid":52208,"wsMB":578,"cpuPct":0.7,"path":"G:\\SteamLibrary\\steamapps\\common\\Slay the Spire 2\\SlayTheSpire2.exe"}]}}
```

### io

输入 `top:2`；实测 2 块盘和 2 条进程记录：

```json
{"io":{"intervalSec":2.22,"disks":[{"disk":"1 C: D: E:","queueLen":0,"busyPct":1,"readKBs":0,"writeKBs":709}],"byIo":[{"name":"chrome","pid":34000,"ioKBs":1454.9}]}}
```

### gpu

输入 `top:2`；实测两份排行各 2 项，2 个适配器、1 条 NVIDIA 状态：

```json
{"gpu":{"byGpuPct":[{"pid":52208,"name":"SlayTheSpire2","gpuPct":5,"engtypes":"3d+copy+jpeg+ofa+security+videodecode+videoencode+vr"}],"byDedicatedMB":[{"pid":1680,"name":"dwm","dedicatedMB":20224}],"adapters":[{"name":"GameViewer Virtual Display Adapter","vendor":"GameViewer","driver":"15.6.5.199","status":"OK","bus":"ROOT"}],"nvidia":[{"name":"NVIDIA GeForce RTX 5070 Ti","tempC":50,"powerW":37.51,"vramUsedMB":12881,"vramTotalMB":16303,"utilPct":9,"driver":"591.86"}],"counterErrors":{"engine":null,"memory":null}}}
```

### sensor

```json
{"sensor":{"admin":false,"pawnio":false,"sensorCount":6,"sensors":[{"hw":"NVIDIA GeForce RTX 5070 Ti","name":"GPU Core","type":"Temperature","value":49.42}],"thermalZones":[{"zone":"\\_tz.tz00","tempC":27.9,"passivePct":100}],"frequency":{"cores":16,"avgPctOfMax":95.1,"minPctOfMax":86}}}
```

## 特殊情况

`sensor` 实测 `admin:false`、`pawnio:false`，仍取得 6 条传感器读数及热区和频率数据。卡片只显示已有来源；没有 CPU/主板温度时应保留 `notice` 的采集范围，不能推断温度正常。

`overview` 使用醒目例外画布：上方实测同时含 CPU、内存、页面文件、内核池、机型与开机时长，需在同次结果里突出整体负载。它复用字段组与通用状态行。`gpu` 的 `nvidia:null`、`nvidia:{error}` 和 `lhmGpu` 分支由代码确认，本机实测返回的是 `nvidia[]`。整次 `isError` 则改用原文块。
