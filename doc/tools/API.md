# 开放工具接口

本文定义 `crates/cst-kit` 开放的 14 个工具的通用返回契约、输入参数、输出字段，以及配套的联网检索配置与方案导出 HTTP 接口。行为约束见 [SPEC.md](SPEC.md)。

## 通用返回与体积契约

全部 14 个工具同时返回供模型消费的 `content` 块与供界面渲染的 `details` 结构化对象：

```json
{
  "content": [{ "type": "text", "text": "<序列化 JSON>" }],
  "details": { "elapsedMs": 12 }
}
```

图片读取返回双块：`content` 为 `[Text(meta_json), Image(base64)]`，`details` 仅包含 `meta_json`（含 `elapsedMs`，不含图片 base64）。

1. **统一耗时 `elapsedMs`**：全部 14 个工具在执行阶段（含正常、降级、执行期结构化错误与裁剪输出）顶层均包含整数毫秒 `elapsedMs`；运行时前置参数 Schema 校验错误在进入执行前拦截，不经计时包装。Web 与桌面端卡片在重放历史会话时优先使用事件 `durationMs`，缺失时回退 `details.elapsedMs`。
2. **模型文本 `50 KiB` 受保护键裁剪**：序列化 UTF-8 字节超过 `51,200` 字节时，`result.rs` 基于完整 `details` 单次递减裁剪大数组与长字符串并输出合法 JSON，顶层附加 `outputTruncated: true`、`originalBytes` 与 `outputNotice`，优先保留 `elapsedMs`、`degraded`、`degradedFrom`、`error`、`notice`、`file`、`dir`、`name`、`runbookId`、`version`、`sequence`、`action`、`level`、`levelLabel`、`items` 等核心键（极端退路仅保留最小对象，第二轮允许缩减长 `error` / `notice`）。`details` 保留工具执行后的完整未裁剪对象。
3. **状态与错误字段**：
   - 执行期返回 `Err`（整次失败）：包装为 `isError: true`，文本与 `details` 为结构化 JSON `{"error": "...", "elapsedMs": N}`；前置参数 Schema 校验错误在进入执行前拦截，不附 `elapsedMs`。
   - 局部降级与不完整：返回 `isError: false`，按具体工具输出 `degraded: true` 及对应的错误明细（如 `collectionErrors`、`counterErrors`、`smartErrors`、`degradedFrom`）；部分场景（如 `eventlog(security)` 非管理员或 `disk(health)` 无可用 SMART、`bitlocker` 业务拒绝／取消）可在无错误数组或全部目标被拒时返回 `isError: false`，`isError: false` 不等价于测量完整或操作成功。
   - 正常调用：省略 `degraded` 与空错误容器，不输出 `degraded: false`、`counterErrors: {}` 或 `collectionErrors: []`。
   - `notice` / `*Notice`：仅描述数据来源、单位、采样窗口与采集限制。

## read

- **参数**：`{ path: string, offset?: number, limit?: number }`（`offset` 1-based，默认 `1`；`limit` 默认 `2000`）。
- **返回（顶层对象）**：
  - 文本文件：`{ path, kind: "text", offset, limit, outputLines, totalLines, outputBytes, truncated, truncatedBy?: "lines" | "bytes" | "limit", nextOffset?: number, firstLineExceedsLimit?: true, firstLineBytes?: number, encodingDegraded?: true, content, notice?, elapsedMs }`。0 字节空文件返回 `totalLines: 0`、`content: ""`；含空字节的二进制文件与 UTF-16 文件报错拒绝。
  - 图片文件：`content` 为 `[Text(meta_json), Image(data, mimeType)]`，其中 `meta_json` 与 `details` 为 `{ path, kind: "image", mimeType, bytes, resized, hints: string[], imageAttached: boolean, degraded?, collectionErrors?, notice?, elapsedMs }`。单图上限 `20 MiB`，无法解码的损坏图片降级返回单块 JSON（`imageAttached: false`、`degraded: true`）。

## ls

- **参数**：`{ path: string, top?: number }`（`top` 默认 20，最大 50）。
- **返回（顶层对象）**：

| 字段 | 类型与含义 |
|---|---|
| `path` | 规范化目录路径 |
| `totalChildren` / `totalSize` | 可见直接子项总数（不含已隐去敏感路径名）/ 已统计直接子项大小文本（不完整时为下界） |
| `method` | `"native-index"`（全部直接子目录命中同卷有效 `$MFT` 缓存）或 `"walk"`（原生并行目录遍历） |
| `entries[]` | 按归属字节降序排列，`{ name, type: "dir"\|"file"\|"unknown", size: string\|null, bytes: number\|null, pct: number\|null }` |
| `omitted` | 可见项超出 `top` 时存在，`{ count, size, unknownCount, note }` |
| `degraded` / `degradedFrom` | `$MFT` 回退或子项不完整时为 `true` / 附回退原因 |
| `childrenTruncated` | 可选布尔值，直接子项枚举触及预算时为 `true` |
| `notice` / `elapsedMs` | 权限或截断说明 / 执行耗时（毫秒） |

## disk

- **参数**：`{ scope: "space" | "info" | "health" | "usage" | "all", drive?: string, path?: string, top?: number }`（`usage` 必填 `path`，`top` 默认 20、最大 100；其余 `scope` 可选 `drive` 如 `"C"` 或 `"C:"`）。
- **返回（按数据类别挂在顶层，统一含 `elapsedMs`）**：

| `scope` | 返回字段 |
|---|---|
| `space` | `space[]: { drive, totalGB, freeGB, usedPct }[]`、可选 `degraded`、`collectionErrors` |
| `info` | `physicalDisks[]: { FriendlyName, SerialNumber, MediaType, BusType, HealthStatus, OperationalStatus, DeviceId, sizeGB }[]`、`volumes[]: { drive, label, fs, driveType, totalGB, freeGB }[]`、可选 `infoNotice`、`degraded`、`collectionErrors` |
| `health` | `smart: { DeviceId, Wear, Temperature, PowerOnHours, ReadErrorsTotal, WriteErrorsTotal }[] \| null`、可选 `smartErrors[]: { deviceId, error, code? }[]`、`smartNotice`、`degraded` |
| `usage` | `usage: { root, totalGB, method: "native-mft"\|"native-walk", topDirs[]: { path, sizeGB, pct }[], topFiles[]: { path, sizeGB, modified }[], extAgg[]: { ext, files, sizeGB }[]（最多 40 项）, staleFiles[]: { path, sizeGB, modified }[], stats: { filesScanned, budget? }, truncated?, denied[]?, degraded?, degradedFrom?, notice }` |
| `all` | 同层合并 `space`、`info`、`health` 的全部字段 |

## fffind 与 ffgrep

| 工具 | 核心参数 | 返回契约（均含 `elapsedMs`） |
|---|---|---|
| `fffind` | `{ pattern, path?, exclude?, limit?, caseSensitive? }`（`path` 支持目录、文件或通配过滤；`limit` 默认 30、范围 1–1000；不接受 `include`） | `{ matches: { path }[], truncated: boolean, skipped: number, notice?: string, elapsedMs }`；按相关性排序，全弱匹配附 `notice` |
| `ffgrep` | `{ pattern, path?, exclude?, limit?, context?, caseSensitive?, literal? }`（`path` 支持目录、文件或通配过滤；`limit` 默认 30、范围 1–1000；`context` 0–20 且计入返回条数；不接受 `include`） | `{ matches: { path, line, text, matched }[], truncated: boolean, skipped: number, skippedLargeFiles?: number, degraded?: boolean, notice?: string, elapsedMs }`；支持 UTF-16 LE/BE BOM 与 GB18030，单文件限 `10 MiB`，总读取达 `256 MiB` 时保留命中并置 `truncated: true`、`degraded: true` |

## sys

- **参数**：`{ scope?: "overview" | "proc" | "gpu" | "io" | "sensor", top?: number }`（默认 `overview`；`top` 作用于 `proc` / `gpu` / `io`，默认 10、最大 50）。
- **返回（顶层键为 `[scope]`，含 `notice`、`elapsedMs` 及可选 `degraded` / `collectionErrors`）**：

| `scope` | 主要字段 |
|---|---|
| `overview` | `cpuTotalPct`（200 ms `GetSystemTimes` 双采）、`logicalCores`、`mem: { totalMB, usedMB, freeMB, usedPct }`、`pagefile[]: { name, allocMB, usedMB, peakMB }[]`、`pool: { nonpagedMB, pagedMB }`、`machine: { vendor, model, cpu, physicalCores, bios, biosDate }`、`uptime: { bootTime, text, totalHours }` |
| `proc` | `cores`、`totalProcs`、`intervalSec`、`byCpu[]: { name, pid, wsMB, cpuPct, path }[]`、`byMem[]`（同结构；`NtQuerySystemInformation(5)` 全量采样，仅 Top-N 填充 `path`） |
| `gpu` | `byGpuPct[]: { pid, name: string\|null, gpuPct, engtypes, engines[]: { adapter, type, gpuPct }[] }[]`（`engines` 仅保留 `gpuPct > 0`）、`byDedicatedMB[]: { pid, name: string\|null, dedicatedMB }[]`、`engineSamples`、`adapters[]: { name, vendor, driver, status, bus }[]`、`nvidia: { name, uuid, pciBusId, tempC, powerW, vramUsedMB, vramTotalMB, utilPct, driver }[] \| { error } \| null`、可选 `lhmGpu: { hardware, sensors: { hw, name, type, unit, value }[], error?, degraded?, collectionErrors? }`、可选非空 `counterErrors` |
| `io` | `intervalSec`、`totalProcs`、`disks[]: { disk, queueLen, busyPct, readKBs, writeKBs }[]`（PDH `PhysicalDisk`，排除 `_Total`）、`byIo[]: { name, pid, ioKBs }[]`（`NtQuerySystemInformation(5)` 双采差分） |
| `sensor` | `admin`、`pawnio`、`hardware[]`、`sensorCount`、`sensors[]: { hw, name, type, unit, value }[]`、`thermalZones[]: { zone, tempC, passivePct }[]`、`frequency: { cores, minPctOfMax, avgPctOfMax }`、可选非空 `counterErrors` |

## startup

- **参数**：`{}`（无参数）。
- **返回（顶层键为 `startup`，含 `elapsedMs`）**：
  - `regItems[]: { source, name, command, disabled: boolean | null }[]`
  - `startupFolders[]: { scope: "user" | "allUsers", path, items: { name, disabled: boolean | null }[] }[]`
  - `services[]: { name, display, state, path }[]`（优先 Win32 SCM，`Running` 排前，`path` 超 140 字符截断附 `…`）
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
| `logName` / `ids` / `provider` | `logName` 用于 `query`（默认 `"System"`）与 `detail`（必填）；`ids` 仅 `query`；`provider` 作用于全部列表类 `scope` | 通道名（1–100 字节 ASCII 字母数字开头，其余仅字母数字、空格及 `./_-`）/ 最多 16 个 `0–65535` 整数 / 提供程序正则（1–128 字节，忽略大小写，按本地正则匹配） |
| `recordId` / `id` | `detail` | 二选一必填。`recordId`（非负整数）匹配精确记录号，`id` 取该事件 ID 最新一条，同时必须提供 `logName` |

- **返回（顶层键为 `[scope]`，含 `elapsedMs`）**：
  - 列表类 `scope`：`logs`、`hours`、`top`、`total`、`truncated`、`events[]: { logName, time, recordId, level, levelName, provider, id, msg }[]`（`time` 为本机带偏移 RFC3339，`msg` 最多 200 字符，未读到时为 `null`）、`counts[]: { key, provider, id, n, last }[]`（先收录扫描遇到的至多 100 组再按 `n` 降序）、`countsTruncated`、可选 `scanTruncated`、`unreadable`、`firstTime`、`lastTime`、`admin: boolean | null`、`notice`、可选 `noMatch` / `degraded` / `collectionErrors`。所有请求通道全部失败返回 `isError: true`。
  - `detail`：`{ found: boolean, logName, recordId?, id?, time?, level?, levelName?, provider?, machine?, msg?, msgTruncated?, originalChars?, notice, unreadable?, degraded? }`（`msg` 保留换行，最多 20,000 字符，超限置 `msgTruncated: true` 与 `originalChars`，未读到消息时为 `null` 且置 `degraded: true`）。

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

- **参数**：`{ scope?: "problem" | "core" | "external" | "find", name?: string, class?: string, id?: string }`（默认 `problem`；`find` 至少传 `name` / `class` / `id` 之一）。
- **返回（顶层键为 `[scope]`，含 `notice`、`elapsedMs` 及可选 `degraded` / `collectionErrors`）**：

| `scope` | 主要字段 |
|---|---|
| `problem` / `find` | `count`、`devices[]: { name, class, status, errorCode, deviceId, hardwareIds[] }[]` |
| `core` | `net[]: { name, connId, physical, connStatus }[]`、`bluetooth[]: { name, status, errorCode }[]`、`audio[]`（同结构）、`display[]: { name, vendor, driver, status, bus }[]`、`services[]: { name, state }[]`（`bthserv`、`Audiosrv`，优先 SCM）、`drivers[]: { class, device, version, date, provider }[]`（优先 SetupAPI，`date` 格式化为 `YYYY-MM-DD`） |
| `external` | `devices[]: { name, class, status, errorCode, deviceId }[]`、`removable[]: { model, interface, mediaType, sizeGB }[]` |

## 联网检索工具

| 工具 | 核心参数 | 返回契约（均含 `elapsedMs`） |
|---|---|---|
| `web_search` | `{ query?, queries?, numResults?, mode?: "fast"\|"auto"\|"deep", includeDomains?, excludeDomains?, afterDate?, beforeDate?, languages?, contentMode?: "highlights"\|"summary"\|"text", additionalQueries?, oriQuestion?, provider?: "exa"\|"deepseek" }`（1–3 词） | `{ responseId, provider, mode, queryCount, successfulQueries, queries: { query, ok, resultCount, results: { title, url, snippet, excerpt, publishedAt?, score?, authority?, siteName? }[], preview?, synthesis?, grounding?, error? }[], degraded?, degradedFrom?, collectionErrors?, notice?, elapsedMs }`；并发执行，单条失败局部降级，全败返 `isError: true` |
| `web_fetch` | `{ url?, urls?, mode?: "auto"\|"provider"\|"direct", maxCharacters?, provider?: "exa"\|"deepseek" }`（1–5 链接） | `{ responseId, provider, fetchMode, urlCount, successful, urls: { url, ok, title?, totalCharacters?, truncated?, content, error? }[], degraded?, collectionErrors?, elapsedMs }`；并发执行，直连与云端抓取前均经本地 SSRF 策略与 DNS 预检，直连支持 UTF-8 与 GB18030 |
| `get_search_content` | `{ responseId, offset?, limit?, findText? }`（单次最多 6000 字符） | `{ responseId, mode: "range" \| "find", content, totalCharacters, returnedCharacters, offset, nextOffset?, truncated, matchCount?, elapsedMs }`（`find` 模式零命中返 `matchCount: 0`） |

### web-search 配置与服务端接口

配置文件位于 `data/config/web-search.json`（v2，兼容读取 v1，上限 `64 KiB`），默认启用 `exa`（含免费 MCP 回退），可选切换 `deepseek`（官方 `https://api.deepseek.com` 支持 `reuseModelApiKey`）。服务端路由位于 `cst-server`：

| 方法与路径 | 用途与安全规则 |
|---|---|
| `GET /api/settings/web-search` | 读取当前联网检索配置脱敏视图（返回 `hasApiKey` 与 `apiKeyTail`，绝不返回明文密钥） |
| `PATCH /api/settings/web-search` | 按字段合并更新配置并校验落盘 `≤ 64 KiB` 后原子写入。清空密钥须显式传 `clearApiKey: true`，拒绝空字符串或脱敏占位回写；`baseUrl` 仅允许 HTTPS，更改来源禁止继承旧 Key；热生效，支持 `Idempotency-Key` |
| `POST /api/settings/web-search/test` | 使用当前或传入参数执行低开销探测，验证 `exa` 或 `deepseek` 连通性与凭据状态；变更 `baseUrl` 来源时不携带已保存密钥 |

## runbook

- **参数**：
  - `action?: "plan" | "export"`（默认 `"plan"`）。
  - `plan`：`title`（1–48 字）、`level: "safe" | "low" | "confirm"`、`items: { summary, command, shell: "cmd" | "powershell", admin?: boolean }[]`（1–20 条）、可选 `date`（`YYYY-MM-DD`）、可选 `runbookId` 与 `baseVersion`（修订已有方案时成对传入）、可选 `exportTxt: boolean`（默认 `false`）；禁止传 `version`。
  - `export`：仅接受 `runbookId: string` 与 `version: number`（须为最新版本），禁止同传方案编辑字段。
- **返回（顶层键为 `runbook`，含 `elapsedMs`）**：
  - `{ runbookId, version, sequence, day, createdAt, timeText, title, level, levelLabel, items, commands[], action: "planned" | "exported", file?, dir?, name?, bytes?, encoding?, notice }`。
  - `commands[]` 统一为 `{ summary, command, shell: "cmd" | "powershell", admin: boolean }`。导出路径统一为 `artifacts/<YYYY-MM-DD>/runbook/<序号:02>-<风险档>-<标题>-v<版本>.txt`。
- **服务端一键导出接口**：
  - `POST /api/sessions/:id/runbook/export`：请求体 `{ entryId, toolCallId, open?: boolean }`（`open` 须为布尔值，默认 `true`），按该卡片记录的 `runbookId` 与 `version` 导出 `.txt`（支持核对历史卡片版本），按 `open` 校验无重解析点与硬链接后调用记事本打开，返回 `{ runbook, launched, launchError? }`（记事本启动失败仍保留已导出的 `runbook` 与 `launched: false`）。
  - `POST /api/sessions/:id/runbook/open`：校验无重解析点与硬链接后打开已导出的 `.txt` 清单（兼容历史 `runbook_deliver` 记录）。

## bitlocker

- **参数**：`{ scope?: "status" | "unlock" | "decrypt", drive?: string, recoveryPassword?: string }`（默认 `status`；`unlock` 必填 `drive`，可选 8 组 6 位 ASCII 数字以 `-` 分隔的 `recoveryPassword`；其他 `scope` 禁止传 `recoveryPassword`）。
- **返回（顶层键为 `bitlocker`，含 `elapsedMs`）**：
  - 查询级：`{ scope, collectedAt, admin, source: "Win32_EncryptableVolume", volumes[], notice, degraded?, collectionErrors?, operation? }`（无错误时不输出 `collectionErrors` 与 `degraded`）。
  - `volumes[]`：`{ volumeId, drive, volumeType, conversionStatus, encryptionPercentage, encryptionMethod, protectionStatus, lockStatus, autoUnlockEnabled, autoUnlockKeysStored, keyProtectors: { id, type }[], collectionErrors? }`。
  - `unlock` / `decrypt` 在每卷附加 `operation`：`{ status: "accepted" | "rejected" | "skipped" | "cancelled" | "unknown", reason, returnCode: number | null, error: { source, code, message } | null, credentialSource?: "localInput" | "toolParameter" }`。
  - 进程崩溃重启后的自动复查在顶层附加 `operation: { status: "unknown", reason }` 并持久化 `cst.bitlocker-state-verification` 自定义条目。
