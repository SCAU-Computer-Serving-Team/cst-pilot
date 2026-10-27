# 诊断工具接口

本文定义 `diagnostics` 模块 10 个工具的通用返回结构、输入参数与输出字段契约。行为约束见 [SPEC.md](SPEC.md)。

## 通用返回与体积契约

所有工具同时返回给模型消费的 `content` 文本与给界面渲染的 `details` 对象：

```ts
{ content: [{ type: "text", text: JSON.stringify(result) }], details: result }
```

1. **模型文本 50 KiB 上限**：序列化后超过 50 KiB 时，`result.ts` 先裁剪列表项与长字符串再生成合法 JSON，并附加 `outputTruncated: true`、`originalBytes` 与 `outputNotice`；`details` 保留完整未裁剪数据供 Web 卡片与 TUI 渲染。
2. **状态与错误字段**：
   - `notice` / `*Notice`：数据口径、单位、采样窗口与权限／截断说明。
   - `degraded: true`：部分数据源采集失败，其余成功字段仍可使用。
   - `collectionErrors` / `counterErrors` / `smartErrors`：子数据源的具体错误信息。
   - `error`：单个子模块或当前查询失败原因；整次调用失败时直接抛错并标记 `isError: true`。
3. **提示词要求**：同名覆盖 Pi 内置工具（如 `ls`）时必须显式提供 `promptSnippet`，确保工具出现在系统提示词可用列表中。

## ls

- **参数**：`{ path: string, top?: number }`（`top` 默认 20，最大 50）。
- **返回（顶层对象）**：

| 字段 | 类型与含义 |
|---|---|
| `path` | 规范化目录路径 |
| `totalChildren` / `totalSize` | 直接子项总数 / 已统计子项大小文本（不完整时为下界） |
| `method` | `"wiztree-index"`（命中共享缓存）或 `"walk"`（递归统计） |
| `entries[]` | 按字节降序：`{ name, type: "dir"\|"file"\|"unknown", size: string\|null, bytes: number\|null, pct: number\|null }` |
| `omitted` | 截断时存在：`{ count, size, unknownCount, note }` |
| `notice` | 预算截断或权限受限说明 |

## disk

- **参数**：`{ scope: "space" | "info" | "health" | "usage" | "all", drive?: string, path?: string, top?: number }`（`usage` 必填 `path`，`top` 默认 20、最大 100；其余 `scope` 可选 `drive` 如 `"C"` 或 `"C:"`）。
- **返回（按数据类别挂在顶层）**：

| `scope` | 返回字段 |
|---|---|
| `space` | `space[]: { drive, totalGB, freeGB, usedPct }[]` |
| `info` | `physicalDisks[]: { FriendlyName, SerialNumber, MediaType, BusType, HealthStatus, OperationalStatus, DeviceId, sizeGB }[]`、`volumes[]: { drive, label, fs, driveType, totalGB, freeGB }[]`、可选 `infoNotice`、`degraded` |
| `health` | `smart: { DeviceId, Wear, Temperature, PowerOnHours, ReadErrorsTotal, WriteErrorsTotal, ... }[] \| null`、可选 `smartErrors[]: { deviceId, error }[]`、`smartNotice`、`degraded` |
| `usage` | `usage: { root, totalGB, method: "wiztree-mft"\|"wiztree-walk"\|"node-walk", topDirs[]: { path, sizeGB, pct }[], topFiles[]?, extAgg[]: { ext, files, sizeGB }[]?, staleFiles[]: { path, sizeGB, lastWrite }[]?, stats?, truncated?, denied[]?, degradedFrom?, notice }` |
| `all` | 同层合并 `space`、`info`、`health` 的全部字段 |

## sys

- **参数**：`{ scope?: "overview" | "proc" | "gpu" | "io" | "sensor", top?: number }`（默认 `overview`；`top` 作用于 `proc`/`gpu`/`io`，默认 10、最大 50）。
- **返回（包装在 `result[scope]` 下，均含 `notice` 及可选 `degraded` / `collectionErrors`）**：

| `scope` | 主要字段 |
|---|---|
| `overview` | `cpuTotalPct`、`logicalCores`、`mem: { totalMB, usedMB, freeMB, usedPct }`、`pagefile[]: { name, allocMB, usedMB, peakMB }[]`、`pool: { nonpagedMB, pagedMB }`、`machine: { vendor, model, cpu, physicalCores, bios, biosDate }`、`uptime: { bootTime, text, totalHours }` |
| `proc` | `cores`、`totalProcs`、`intervalSec`、`byCpu[]: { name, pid, wsMB, cpuPct, path }[]`、`byMem[]`（同结构） |
| `gpu` | `byGpuPct[]: { pid, name, gpuPct, engtypes, engines[] }[]`、`byDedicatedMB[]: { pid, name, dedicatedMB }[]`、`engineSamples`、`adapters[]: { name, vendor, driver, status, bus }[]`、`nvidia: { name, uuid, pciBusId, tempC, powerW, vramUsedMB, vramTotalMB, utilPct, driver }[] \| { error } \| null`、可选 `lhmGpu: { hardware, sensors }`、`counterErrors: { engine, memory }` |
| `io` | `intervalSec`、`totalProcs`、`disks[]: { disk, queueLen, busyPct, readKBs, writeKBs }[]`、`byIo[]: { name, pid, ioKBs }[]` |
| `sensor` | `admin`、`pawnio`、`hardware[]`、`sensorCount`、`sensors[]: { hw, name, type, value }[]`、`thermalZones[]: { zone, tempC, passivePct }[]`、`frequency: { cores, minPctOfMax, avgPctOfMax } \| null`、`counterErrors` |

## startup

- **参数**：`{}`（无参数）。
- **返回（包装在 `result.startup` 下）**：
  - `regItems[]: { source, name, command, disabled: boolean | null }[]`
  - `startupFolders[]: { scope: "user" | "allUsers", path, items: { name, disabled: boolean | null }[] }[]`
  - `services[]: { name, display, state, path }[]`（`Running` 排前，`path` 超 140 字符截断）
  - `notice`、可选 `degraded`、`collectionErrors`

## eventlog

- **参数**：

| 参数 | 适用 `scope` | 类型与约束 |
|---|---|---|
| `scope` | 全部 | `"recent"`（默认）、`"boot"`、`"crash"`、`"service"`、`"disk"`、`"security"`、`"query"`、`"detail"` |
| `hours` / `top` | 除 `detail` 外 | 查询时间窗（默认 24，最大 720 小时）/ 返回样本数（默认及最大 100） |
| `level` | `recent`、`query` | `"warn"`（Critical/Error/Warning，`recent` 默认）或 `"error"`（Critical/Error）；`query` 省略时不限级别 |
| `kind` | `boot` | `"all"`（默认）、`"unexpected"`、`"bluescreen"` |
| `type` | `security` | `"all"`（默认）、`"logonFail"`、`"lockout"` |
| `app` / `name` / `msg` | `crash` / `service` / `query` | 消息字面子串（不区分大小写） |
| `logName` / `ids` / `provider` | `query`（`logName` 亦用于 `detail`） | 通道名 / `0–65535` 整数数组 / 提供程序正则（忽略大小写，1 秒超时） |
| `recordId` / `id` | `detail` | 二选一必填：`recordId` 精确记录号，或 `id` 取该事件 ID 最新一条；同时必须提供 `logName` |

- **返回（包装在 `result[scope]` 下）**：
  - 列表类 `scope`：`logs`、`hours`、`top`、`total`、`truncated`、`events[]: { logName, time, recordId, level, levelName, provider, id, msg }[]`（`msg` 最多 200 字符）、`counts[]: { key, provider, id, n, last }[]`（最多 100 组）、`countsTruncated`、`unreadable`、`firstTime`、`lastTime`、`admin`、`notice`、可选 `noMatch` / `degraded`。
  - `detail`：`{ found: boolean, logName, recordId?, id?, time?, level?, levelName?, provider?, machine?, msg?, notice }`（`msg` 保留换行，最多 20,000 字符）。

### eventlog 内置事件范围

| `scope` | 事件 ID 与提供程序 |
|---|---|
| `boot all` | System：12、13、6005、6006、6009、41、6008、1001、1074、19、7045，以及 `Microsoft-Windows-WHEA-Logger` 全部事件 |
| `boot unexpected` | System：41、6008 |
| `boot bluescreen` | System：1001 + `Microsoft-Windows-WHEA-Logger` |
| `crash` | Application：`Application Error`/1000、`.NET Runtime`/1026（限 Error 级）；`Windows Error Reporting`/1001、`Application Hang`/1002、`SideBySide`/33, 35（不限级别） |
| `service` | System（Service Control Manager）：7000、7001、7002、7003、7009、7011、7013、7022、7023、7024、7025、7026、7031、7032、7034、7038、7041、7043 |
| `disk` | System：7、11、50、51、55、98、129、140、153、157 |
| `security` | Security：4624（登录成功）、4625（`logonFail`）、4740（`lockout`） |

## driver

- **参数**：`{ scope?: "problem" | "core" | "external" | "find", name?: string, class?: string, id?: string }`（默认 `problem`；`find` 至少传 `name` / `class` / `id` 之一）。
- **返回（包装在 `result[scope]` 下）**：

| `scope` | 主要字段 |
|---|---|
| `problem` / `find` | `count`、`devices[]: { name, class, status, errorCode, deviceId, hardwareIds[] }[]`、`notice` |
| `core` | `net[]: { name, connId, physical, connStatus }[]`、`bluetooth[]: { name, status, errorCode }[]`、`audio[]`（同左）、`display[]: { name, vendor, driver, status, bus }[]`、`services[]: { name, state }[]`（`bthserv`、`Audiosrv`）、`drivers[]: { class, device, version, date, provider }[]` |
| `external` | `devices[]: { name, class, status, errorCode, deviceId }[]`、`removable[]: { model, interface, mediaType, sizeGB }[]` |

## command_plan 与 runbook_deliver

| 工具 | 参数 | 返回 `result.runbook` |
|---|---|---|
| `command_plan` | `title`、`level`、`items`；修订时同时传 `runbookId` 与 `baseVersion` | `runbookId`（UUID）、递增 `version`、`day`、`createdAt`、`timeText`、`title`、`level`、`levelLabel`、完整 `commands[]`、`items` 条数、`action: "planned"` |
| `runbook_deliver` | `runbookId: string`、`version: number` | 上述方案字段，加 `file`、`dir`、`name`、`bytes`、`encoding: "utf-8-bom"`、`action: "delivered"` |

`items` 参数沿用下方命令条目约束。`commands[]` 返回规范化的 `{ summary, command, shell, admin }`，`admin` 始终为布尔值。修订版本冲突、方案不存在、过期版本导出、已导出文本被修改时调用失败。

## runbook

- **参数**：
  - `title: string`（必填，1–48 字，自动过滤文件名非法字符）
  - `level: "safe" | "low" | "confirm"`（必填）
  - `sequence?: number`（可选，1–999，指定时覆盖修正已有序号清单）
  - `items: { summary: string, command: string, shell: "cmd" | "powershell", admin?: boolean }[]`（必填，1–20 条；`summary` ≤300 字，`command` 单行 ≤2000 字）
- **返回（包装在 `result.runbook` 下）**：`{ title, action: "created" | "replaced", file, dir, name, sequence, level, levelLabel, items, commands[], bytes, encoding: "utf-8-bom", notice }`。`runbook` 提供单步写入；多轮修订使用 `command_plan` 与 `runbook_deliver`。

## bitlocker

- **参数**：`{ scope?: "status" | "unlock" | "decrypt", drive?: string, recoveryPassword?: string }`（默认 `status`；`unlock` 必填 `drive`，可选 48 位 `recoveryPassword` `XXXXXX-XXXXXX-...`；其他 `scope` 禁止传 `recoveryPassword`）。
- **返回（包装在 `result.bitlocker` 下）**：
  - 查询级：`{ scope, collectedAt, admin, source: "Win32_EncryptableVolume", volumes[], notice, degraded?, collectionErrors? }`。
  - `volumes[]` 每项状态：`{ volumeId, drive, volumeType, conversionStatus, encryptionPercentage, encryptionMethod, protectionStatus, lockStatus, autoUnlockEnabled, autoUnlockKeysStored, keyProtectors: { id, type }[], collectionErrors[] }`。
  - `unlock` / `decrypt` 在每卷附加 `operation`：`{ status: "accepted" | "rejected" | "skipped" | "cancelled" | "unknown", reason, returnCode: number | null, error?: { source, code, message }, credentialSource?: "localInput" | "toolParameter" | null }`。
