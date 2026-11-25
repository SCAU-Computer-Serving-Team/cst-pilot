# 诊断与受控工具接口

本文定义 `diagnostics` 模块开放工具的通用返回结构、输入参数与输出字段契约。行为约束见 [SPEC.md](SPEC.md)。

## 通用返回与体积契约

所有工具同时返回供模型消费的 `content` 文本与供界面卡片渲染的 `details` 结构化对象：

```json
{
  "content": [{ "type": "text", "text": "<序列化 JSON>" }],
  "details": {}
}
```

1. **模型文本 50 KiB 上限**：序列化 UTF-8 字节超过 50 KiB（`51,200` 字节）时，`result.rs` 按 `256 -> 128 -> 64 ...` 递减裁剪数组长度与长字符串后输出合法 JSON，并在顶层附加 `outputTruncated: true`、`originalBytes` 与 `outputNotice`。`details` 保留完整未裁剪对象。
2. **状态与错误字段**：
   - `notice` / `*Notice`：数据口径、单位、采样窗口与权限／截断说明。
   - `degraded: true`：部分数据源采集失败，其余成功字段仍可使用。
   - `collectionErrors` / `counterErrors` / `smartErrors`：子数据源的具体错误信息。
   - `error`：单个子模块失败原因。整次调用失败时标记 `isError: true`。
3. **提示词声明**：同名覆盖内置工具（如 `ls`）时显式声明 `prompt_snippet`，确保工具写入系统提示词 `<tools>` 段落。

## ls

- **参数**：`{ path: string, top?: number }`（`top` 默认 20，最大 50）。
- **返回（顶层对象）**：

| 字段 | 类型与含义 |
|---|---|
| `path` | 规范化目录路径 |
| `totalChildren` / `totalSize` | 可见直接子项总数（不含已隐去的工具包敏感路径名）/ 已统计直接子项大小文本（含已隐去敏感路径的已知大小，不完整时为下界） |
| `method` | `"native-index"`（命中共享卷缓存且未遍历子目录）或 `"walk"`（原生并行遍历，可能同时使用部分缓存） |
| `entries[]` | 按归属字节降序排列（同目录本地 NTFS 硬链接按确定性规则去重），元素为 `{ name, type: "dir"\|"file"\|"unknown", size: string\|null, bytes: number\|null, pct: number\|null }`，`pct` 以 `totalSize` 对应总量为分母 |
| `omitted` | 可见项超出 `top` 截断时存在，结构为 `{ count, size, unknownCount, note }` |
| `notice` | 预算截断、权限受限或敏感路径隐藏说明 |
| `childrenTruncated` | 可选布尔值，直接子项枚举达到预算时为 `true`，此时 `totalChildren` 仅覆盖已枚举的可见项 |

## disk

- **参数**：`{ scope: "space" | "info" | "health" | "usage" | "all", drive?: string, path?: string, top?: number }`（`usage` 必填 `path`，`top` 默认 20、最大 100。其余 `scope` 可选 `drive` 如 `"C"` 或 `"C:"`）。
- **返回（按数据类别挂在顶层）**：

| `scope` | 返回字段 |
|---|---|
| `space` | `space[]: { drive, totalGB, freeGB, usedPct }[]`、可选 `degraded`、`collectionErrors` |
| `info` | `physicalDisks[]: { FriendlyName, SerialNumber, MediaType, BusType, HealthStatus, OperationalStatus, DeviceId, sizeGB }[]`、`volumes[]: { drive, label, fs, driveType, totalGB, freeGB }[]`、可选 `infoNotice`、`degraded`、`collectionErrors` |
| `health` | `smart: { DeviceId, Wear, Temperature, PowerOnHours, ReadErrorsTotal, WriteErrorsTotal, ... }[] \| null`、可选 `smartErrors[]: { deviceId, error, code? }[]`、`smartNotice`、`degraded` |
| `usage` | `usage: { root, totalGB, method: "native-mft"\|"native-walk", topDirs[]: { path, sizeGB, pct }[], topFiles[]: { path, sizeGB, modified }[]?, extAgg[]: { ext, files, sizeGB }[]?（最多 40 项）, staleFiles[]: { path, sizeGB, modified }[]?, stats?: { filesScanned, elapsedSec, budget }, truncated?, denied[]?, degradedFrom?, notice }` |
| `all` | 同层合并 `space`、`info`、`health` 的全部字段 |

## sys

- **参数**：`{ scope?: "overview" | "proc" | "gpu" | "io" | "sensor", top?: number }`（默认 `overview`。`top` 作用于 `proc`/`gpu`/`io`，默认 10、最大 50）。
- **返回（顶层键为 `[scope]`，均含 `notice` 及可选 `degraded` / `collectionErrors`）**：

| `scope` | 主要字段 |
|---|---|
| `overview` | `cpuTotalPct`、`logicalCores`、`mem: { totalMB, usedMB, freeMB, usedPct }`、`pagefile[]: { name, allocMB, usedMB, peakMB }[]`、`pool: { nonpagedMB, pagedMB }`、`machine: { vendor, model, cpu, physicalCores, bios, biosDate }`、`uptime: { bootTime, text, totalHours }` |
| `proc` | `cores`、`totalProcs`、`intervalSec`、`byCpu[]: { name, pid, wsMB, cpuPct, path }[]`、`byMem[]`（同结构） |
| `gpu` | `byGpuPct[]: { pid, name: string\|null, gpuPct, engtypes, engines[]: { adapter, type, gpuPct }[] }[]`、`byDedicatedMB[]: { pid, name: string\|null, dedicatedMB }[]`、`engineSamples`、`adapters[]: { name, vendor, driver, status, bus }[]`、`nvidia: { name, uuid, pciBusId, tempC, powerW, vramUsedMB, vramTotalMB, utilPct, driver }[] \| { error } \| null`、可选 `lhmGpu: { hardware, sensors, notice?, degraded?, collectionErrors? }`、`counterErrors: { engine?: string, memory?: string }` |
| `io` | `intervalSec`、`totalProcs`、`disks[]: { disk, queueLen, busyPct, readKBs, writeKBs }[]`、`byIo[]: { name, pid, ioKBs }[]` |
| `sensor` | `admin`、`pawnio`、`hardware[]`、`sensorCount`、`sensors[]: { hw, name, type, value }[]`、`thermalZones[]: { zone, tempC, passivePct }[]`、`frequency: { cores: number, minPctOfMax: number\|null, avgPctOfMax: number\|null }`、`counterErrors` |

热区、频率与 LHM 独立采集。辅助程序或 DLL 缺失时保留已读取的计数器并标记 `degraded`。权限或 PawnIO 注册项读取失败时对应字段置 `null`，原因写入 `collectionErrors`。

## startup

- **参数**：`{}`（无参数）。
- **返回（顶层键为 `startup`）**：
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
| `level` | `recent`、`query` | `"warn"`（Critical/Error/Warning，`recent` 默认）或 `"error"`（Critical/Error）。`query` 省略时不限级别 |
| `kind` | `boot` | `"all"`（默认）、`"unexpected"`、`"bluescreen"` |
| `type` | `security` | `"all"`（默认）、`"logonFail"`、`"lockout"` |
| `app` / `name` / `msg` | `crash` / `service` / `query` | 消息字面子串（1–200 字符，不区分大小写） |
| `logName` / `ids` / `provider` | `query`（`logName` 亦用于 `detail`） | 通道名（`query` 默认 `"System"`，`detail` 必填）/ 最多 16 个 `0–65535` 整数 / 提供程序正则（1–128 字节，忽略大小写） |
| `recordId` / `id` | `detail` | 二选一必填。`recordId` 匹配精确记录号，`id` 取该事件 ID 最新一条，同时必须提供 `logName` |

- **返回（顶层键为 `[scope]`）**：
  - 列表类 `scope`：`logs`、`hours`、`top`、`total`、`truncated`、`events[]: { logName, time, recordId, level, levelName, provider, id, msg }[]`（`msg` 最多 200 字符）、`counts[]: { key, provider, id, n, last }[]`（最多 100 组）、`countsTruncated`、可选 `scanTruncated`、`unreadable`、`firstTime`、`lastTime`、`admin`、`notice`、可选 `noMatch` / `degraded`。
  - `detail`：`{ found: boolean, logName, recordId?, id?, time?, level?, levelName?, provider?, machine?, msg?, notice, unreadable?, degraded? }`（`msg` 保留换行，最多 20,000 字符）。

### eventlog 内置事件范围

| `scope` | 事件 ID 与提供程序 |
|---|---|
| `boot all` | System：12、13、6005、6006、6009、41、6008、1001、1074、19、7045，以及 `Microsoft-Windows-WHEA-Logger` 全部事件 |
| `boot unexpected` | System：41、6008 |
| `boot bluescreen` | System：1001 + `Microsoft-Windows-WHEA-Logger` |
| `crash` | Application：`Application Error`/1000、`.NET Runtime`/1026（限 Error 级），以及不限级别的 `Windows Error Reporting`/1001、`Application Hang`/1002、`SideBySide`/33, 35 |
| `service` | System（Service Control Manager）：7000、7001、7002、7003、7009、7011、7013、7022、7023、7024、7025、7026、7031、7032、7034、7038、7041、7043 |
| `disk` | System：7、11、50、51、55、98、129、140、153、157 |
| `security` | Security：4624（登录成功）、4625（`logonFail`）、4740（`lockout`） |

## driver

- **参数**：`{ scope?: "problem" | "core" | "external" | "find", name?: string, class?: string, id?: string }`（默认 `problem`。`find` 至少传 `name` / `class` / `id` 之一）。
- **返回（顶层键为 `[scope]`，均含 `notice` 及可选 `degraded` / `collectionErrors`）**：

| `scope` | 主要字段 |
|---|---|
| `problem` / `find` | `count`、`devices[]: { name, class, status, errorCode, deviceId, hardwareIds[] }[]` |
| `core` | `net[]: { name, connId, physical, connStatus }[]`、`bluetooth[]: { name, status, errorCode }[]`、`audio[]`（同结构）、`display[]: { name, vendor, driver, status, bus }[]`、`services[]: { name, state }[]`（`bthserv`、`Audiosrv`）、`drivers[]: { class, device, version, date, provider }[]` |
| `external` | `devices[]: { name, class, status, errorCode, deviceId }[]`、`removable[]: { model, interface, mediaType, sizeGB }[]` |

## command_plan 与 runbook_deliver

| 工具 | 参数 | 返回顶层键 `runbook` |
|---|---|---|
| `command_plan` | `title`、`level`、`items`、可选 `date`。修订时同时传 `runbookId` 与 `baseVersion` | `runbookId`（UUID）、递增 `version`、`day`、`createdAt`、`timeText`、`title`、`level`、`levelLabel`、完整 `commands[]`、`items` 条数、`action: "planned"`、`notice` |
| `runbook_deliver` | `runbookId: string`、`version: number` | 上述方案字段（不含 `notice`），加 `file`、`dir`、`name`、`bytes`、`encoding: "utf-8-bom"`、`action: "delivered"` |

`commands[]` 返回规范化的 `{ summary, command, shell: "cmd" | "powershell", admin: boolean }`。修订版本冲突或方案不存在时调用失败。导出过期版本或磁盘文本已被现场修改时同样报错。

## runbook

- **参数**：`title`（1–48 字，过滤路径非法字符）、`level: "safe" | "low" | "confirm"`、可选 `sequence`（1–999，覆盖修正已有序号清单）、可选 `date`（`YYYY-MM-DD`，默认当天）、`items: { summary, command, shell: "cmd" | "powershell", admin?: boolean }[]`（1–20 条，`summary` ≤300 字，`command` 单行 ≤2000 字）。
- **返回（顶层键为 `runbook`）**：`{ title, action: "created" | "replaced", file, dir, name, sequence, level, levelLabel, items, commands[], bytes, encoding: "utf-8-bom", notice }`（其中 `commands[].shell` 输出展示名 `"cmd" | "PowerShell"`）。

## bitlocker

- **参数**：`{ scope?: "status" | "unlock" | "decrypt", drive?: string, recoveryPassword?: string }`（默认 `status`。`unlock` 必填 `drive`，可选 48 位 `recoveryPassword`。其他 `scope` 禁止传 `recoveryPassword`）。
- **返回（顶层键为 `bitlocker`）**：
  - 查询级：`{ scope, collectedAt, admin, source: "Win32_EncryptableVolume", volumes[], notice, degraded?, collectionErrors?, operation? }`。
  - `volumes[]`：`{ volumeId, drive, volumeType, conversionStatus, encryptionPercentage, encryptionMethod, protectionStatus, lockStatus, autoUnlockEnabled, autoUnlockKeysStored, keyProtectors: { id, type }[], collectionErrors[] }`。
  - `unlock` / `decrypt` 在每卷附加 `operation`：`{ status: "accepted" | "rejected" | "skipped" | "cancelled" | "unknown", reason, returnCode: number | null, error?: { source, code, message }, credentialSource?: "localInput" | "toolParameter" | null }`（锁定卷解密返回 `0x80310000`，系统卷残留自动解锁密钥返回 `0x80310029`）。
  - 进程崩溃重启后的自动复查在顶层附加 `operation: { status: "unknown", reason }` 并持久化 `cst.bitlocker-state-verification` 自定义条目。

## 受控文件与联网检索工具

| 工具 | 核心参数 | 安全与返回契约 |
|---|---|---|
| `read` | `{ path, offset?, limit? }` | 拦截敏感配置、`.state/`、设备路径与备用数据流。文本单次上限 2000 行 / 50 KiB，图片自动做 EXIF 修正与自适应缩放 |
| `fffind` / `ffgrep` | `{ pattern, path?, exclude?, limit?, context?, caseSensitive?, literal? }`（`limit` 默认 30、最大 1000） | 遵守 `.gitignore` 并跳过受保护凭据文件，返回 `{ matches, truncated: boolean, skipped: number }`（`fffind` 项为 `{ path }`，`ffgrep` 项为 `{ path, line, text, matched }`，文本超 50 KiB 报错提示收窄范围） |
| `web_search` / `source_check` | `{ query?, claim?, queries?, numResults? }`（1–3 词，`numResults` 默认 5、上限 10） | 关闭环境代理并阻断私网与跨私网重定向（仅放行 `198.18.0.0/15`），返回 `{ responseId, queries: { query, preview }[], provider: "exa" }` |
| `fetch_content` | `{ url?, urls? }`（1–5 个 URL） | 经 SSRF 校验抓取网页正文写入会话缓存，返回 `{ responseId, urls: { url, preview }[] }` |
| `get_search_content` | `{ responseId, offset?, limit?, findText? }`（单次最多 6000 字符） | 从会话内存缓存（上限 32 条 / 8 MiB / 单条 2 MiB / TTL 30 分钟）分页或定位读取，返回 `{ responseId, content, totalCharacters, offset }` |
