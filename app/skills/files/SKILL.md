---
name: files
description: 本地文件读取与检索：读文本与图片（read）、找文件路径（fffind）、搜文件内容（ffgrep）。
---

# 本地文件读取与检索（read / fffind / ffgrep）

## 场景选择

| 目标 | 调用 | 结束条件 |
|---|---|---|
| 已知文件路径，读日志、配置或截图 | `read({ path, offset?, limit? })` | 读完所需行区间或图片元信息，若被截断且有 `nextOffset` 则继续分页 |
| 全电脑或指定目录找文件与目录 | `fffind({ path, pattern?, kind?, ext?, maxDepth?, modifiedWithinHours?, minSizeMiB?, maxSizeMiB?, sortBy?, exclude?, caseSensitive?, limit? })` | 找到目标绝对路径、体积与修改时间，或交代跳过与未扫完卷 |
| 在指定目录或单日志中搜错误码或关键字 | `ffgrep({ pattern, path, include?, exclude?, modifiedWithinHours?, literal?, caseSensitive?, context?, limit? })` | 定位匹配文件与行号，再按需对大文件定向 `ffgrep` 或 `read` |

## read：文本分页与图片读取

参数：`path`（必填）、`offset`（可选，1 起正整数，默认 `1`）、`limit`（可选，正整数，默认 `2000`）。

| 返回类型 | 核心字段 | 判读与边界 |
|---|---|---|
| 文本（`kind: "text"`） | `path`、`offset`、`limit`、`outputLines`、`totalLines`、`outputBytes`、`truncated`、`truncatedBy`（`lines` / `bytes` / `limit`）、`nextOffset?`、`firstLineExceedsLimit?`、`firstLineBytes?`、`encodingDegraded?`、`content`、`notice?`、`elapsedMs` | 单次最多输出 2000 行且受 50 KiB JSON 字节预算约束，`limit` 不保证返回同等行数。分页仍会遍历文件统计总行数并检查空字节，不能降低大文件扫描耗时。0 字节文件仅 `offset: 1` 可读（`totalLines: 0`）；`offset` 超总行数返回 `isError: true`。仅在 `nextOffset` 存在时传 `offset: nextOffset` 续读。`firstLineExceedsLimit: true` 返回空正文，若已到末行则无 `nextOffset`，不可重试同一偏移；超过 8 KiB 的行 `ffgrep` 也会跳过，无法读取时请队员提供安全节选。`encodingDegraded: true` 表示实际解码行含非 UTF-8 字节并已替换解码 |
| 图片（`kind: "image"`） | `path`、`mimeType`、`bytes`、`resized`、`hints[]`、`imageAttached`、`degraded?`、`collectionErrors?`、`notice`、`elapsedMs` | 支持 PNG、JPEG、GIF、WebP、BMP，单图上限 20 MiB。解码成功时工具返回文本元信息与图片附件双块（`imageAttached: true`）；当前模型无视觉能力时仍返回 `imageAttached: true`，图片在发给模型时省略并在 `notice` 说明，严禁猜测画面内容。损坏图片降级返回单块 JSON（`imageAttached: false`、`degraded: true`） |

显式含 UTF-16 BOM 或二进制空字节的文件会直接报错（`isError: true`）；带 BOM 的 UTF-16 文本可改用 `ffgrep` 检索。`read` 无 30 秒固定超时。

## fffind：全电脑与目录路径检索

`fffind` 使用独立参数结构并拒绝未知字段，`path` 必填。

| 参数 | 作用说明 |
|---|---|
| `path` | 必填。传 `"*"` 扫描本机全部可见固定盘与可移动盘（跳过光驱与网络盘）；传盘符（如 `"C:"`、`"C:\\"`）、目录或单文件路径扫描指定范围。拒绝 UNC、网络映射盘与内嵌通配符 |
| `pattern` | 可选，UTF-8 上限 4096 字节。支持完整精确名、通配符（`*`、`?`、`[]`、`{}`）与空格多词模糊匹配；省略或传 `"*"`/`"**"` 表示不按名称筛选；空串、纯空白或非法通配符直接报错 |
| `kind` / `ext` | `kind` 可选 `"file"`（默认）、`"dir"`、`"any"`；`ext` 为最后一段扩展名白名单（如 `["dmp", "log"]`，最多 32 项）。`kind: "dir"` 不可与 `ext`、体积或 `sortBy: "size"` 同传 |
| `maxDepth` | 可选整数 `1–1024`，根深度为 0，`1` 仅看直接子项；单文件根不可传 |
| `modifiedWithinHours` | 可选正数（`0 < N <= 87600`），按调用时刻 UTC 闭区间 `[now-N, now]` 筛选，未来与未知时间不命中 |
| `minSizeMiB` / `maxSizeMiB` | 可选非负二进制 MiB（`1 MiB = 1048576 B`），含端点换算整字节；`maxSizeMiB: 0` 仅匹配空文件 |
| `sortBy` | 可选 `"relevance"`、`"modified"`、`"size"`，省略时按名称、时间、大小条件自动推断 |
| `exclude` / `caseSensitive` / `limit` | `exclude` 为排除通配数组（尾 `/` 剪枝目录）；`caseSensitive` 在 Windows 默认 `false`；`limit` 范围 `1–1000`（默认 `30`，越界报错） |

返回字段：`root`（规范绝对路径或 `"*"`）、`scannedRoots?`、`partialRoots?`、`skippedRoots?: { root, reason }[]`、`matches[]: { path, kind, sizeBytes, modified }`、`truncated`、`truncatedReason?`（`"timeout"` / `"output_bytes"` / `"limit"`）、`degraded?`、`skipped`、`notice?`、`elapsedMs`。`matches[].path` 统一输出正斜杠规范绝对路径，可直接传给 `read` 或 `ffgrep`。

## ffgrep：指定目录与单文件大日志内容检索

`ffgrep` 使用独立参数结构并拒绝未知字段，`pattern` 与 `path` 双必填。

| 参数 | 作用说明 |
|---|---|
| `pattern` | 必填，非空，UTF-8 上限 4096 字节，按单行匹配（空白字符作为正文保留） |
| `path` | 必填，目标目录或单个文件路径，拒绝 UNC、网络映射盘与内嵌通配符 |
| `include` / `exclude` | 可选通配数组（最多 64 项），仅目录模式允许传入（单文件传入报错）。`include` 不含 `/` 匹配任意层级文件名；`exclude` 尾 `/` 剪枝目录且优先于 `include` |
| `modifiedWithinHours` | 可选正数（`0 < N <= 87600`），在打开文件前按修改时间过滤旧文件并在句柄上复核 |
| `literal` / `caseSensitive` | `literal` 默认 `false`（设 `true` 字面匹配）；`caseSensitive` 在字面模式下省略时智能大小写，正则模式下省略时固定 `false` |
| `context` / `limit` | `context` 范围 `0–20`（默认 `0`），`limit` 范围 `1–1000`（默认 `30`，越界报错）；触及 `limit` 时优先保留命中行 |

返回字段与边界：

1. **返回字段**：`root`、`matches[]: { path, line, text, matched }`、`truncated`、`truncatedReason?`（优先级 `timeout > read_budget > visited_limit > output_bytes > limit > line_limit`）、`degraded?`、`skipped`、`skippedLargeFiles?`、`notice?`、`elapsedMs`。
2. **遍历与大日志**：两工具不读取 `.gitignore` 或 `.ignore`，仅跳过 `.git`、重解析目录与云占位目录。`ffgrep` 目录模式跳过大于 10 MiB 的单文件（独立计入 `skippedLargeFiles`）；直接将单个大日志路径传给 `ffgrep` 时自动启用 256 KiB 分块流式扫描，支持 UTF-16 LE/BE BOM、UTF-8 与 GB18030，单行上限 8 KiB，总读取上限 256 MiB，30 秒返回期限内保留已命中行。
3. **零命中判读**：查找无结果时先核对 `partialRoots`、`skippedRoots`、`skipped` 与 `skippedLargeFiles`；目录模式跳过大日志时直接对该单文件调用 `ffgrep` 或 `read`。

## 访问拦截范围

三工具会拒读受保护凭据文件（默认位于 `data/config/{auth,models,mcp-auth,web-search}.json` 与 `data/telemetry/identity.json`）、`.state/` 目录及其硬链接或符号链接别名，同时拒绝无法核验的链接、未驻留云文件、Windows 设备路径与 NTFS 备用数据流。跨工作目录路径仍会经过路径检查，不能将工作目录视为访问隔离边界。
