---
name: files
description: 本地文件读取与检索：读文本与图片（read）、找文件路径（fffind）、搜文件内容（ffgrep）。
---

# 本地文件读取与检索（read / fffind / ffgrep）

## 场景选择

| 目标 | 调用 | 结束条件 |
|---|---|---|
| 已知文件路径，读日志、配置或截图 | `read({ path, offset?, limit? })` | 读完所需行区间或图片元信息，若被截断且有 `nextOffset` 则继续分页 |
| 只知文件名片段或大致目录 | `fffind({ pattern, path?, exclude?, limit?, caseSensitive? })` | 找到目标文件路径，或交代被忽略与跳过的范围 |
| 在多份文本中搜错误码或关键字 | `ffgrep({ pattern, path?, exclude?, limit?, context?, caseSensitive?, literal? })` | 定位匹配文件与行号，再按需用 `read` 查上下文 |

## read：文本分页与图片读取

参数：`path`（必填）、`offset`（可选，1 起正整数，默认 `1`）、`limit`（可选，正整数，默认 `2000`）。

| 返回类型 | 核心字段 | 判读与边界 |
|---|---|---|
| 文本（`kind: "text"`） | `path`、`offset`、`limit`、`outputLines`、`totalLines`、`outputBytes`、`truncated`、`truncatedBy`（`lines` / `bytes` / `limit`）、`nextOffset?`、`firstLineExceedsLimit?`、`firstLineBytes?`、`encodingDegraded?`、`content`、`notice?`、`elapsedMs` | 单次最多输出 2000 行且受 50 KiB JSON 字节预算约束，`limit` 不保证返回同等行数。分页仍会遍历文件统计总行数并检查空字节，不能降低大文件扫描耗时。0 字节文件仅 `offset: 1` 可读（`totalLines: 0`）；`offset` 超总行数返回 `isError: true`。仅在 `nextOffset` 存在时传 `offset: nextOffset` 续读。`firstLineExceedsLimit: true` 返回空正文，若已到末行则无 `nextOffset`，不可重试同一偏移；超过 8 KiB 的行 `ffgrep` 也会跳过，无法读取时请队员提供安全节选。`encodingDegraded: true` 表示实际解码行含非 UTF-8 字节并已替换解码 |
| 图片（`kind: "image"`） | `path`、`mimeType`、`bytes`、`resized`、`hints[]`、`imageAttached`、`degraded?`、`collectionErrors?`、`notice`、`elapsedMs` | 支持 PNG、JPEG、GIF、WebP、BMP，单图上限 20 MiB。解码成功时工具返回文本元信息与图片附件双块（`imageAttached: true`）；当前模型无视觉能力时仍返回 `imageAttached: true`，图片在发给模型时省略并在 `notice` 说明，严禁猜测画面内容。损坏图片降级返回单块 JSON（`imageAttached: false`、`degraded: true`） |

显式含 UTF-16 BOM 或二进制空字节的文件会直接报错（`isError: true`）；带 BOM 的 UTF-16 文本可改用 `ffgrep` 检索。`read` 无 30 秒固定超时。

## fffind 与 ffgrep：路径查找与内容检索

两工具共用同一参数结构，拒绝未知字段，不接受 `include` 参数。包含过滤直接写在 `path` 通配表达式中。

| 参数 | 作用说明 |
|---|---|
| `pattern` | 必填，上限 4096 字节。`fffind` 支持空格多词交集，传空或 `*` 列举文件；`ffgrep` 不可为空，默认正则匹配 |
| `path` | 可选，目录路径、文件路径或通配过滤（如 `C:/Logs/**/*.log`）；省略从当前工作目录开始 |
| `exclude` | 可选，排除通配数组，正斜杠匹配，末尾加 `/` 排除整目录 |
| `limit` | 可选，返回条数上限，默认 `30`，范围 `1–1000` |
| `caseSensitive` | 可选，两工具均生效；省略时含大写字母自动区分大小写 |
| `context` | 可选，仅影响 `ffgrep`；命中行前后上下文行数（`0–20`，默认 `0`），上下文行计入返回条数且 `matched: false` |
| `literal` | 可选，仅影响 `ffgrep`；默认 `false`，设 `true` 按字面子串匹配 |

返回字段与停止条件：

1. **`fffind`**：`matches[]: { path }`（按文件名相关性降序）、`truncated`、`skipped`、`notice?`（仅找到弱相关匹配时提示收窄范围）、`elapsedMs`。
2. **`ffgrep`**：`matches[]: { path, line, text, matched }`（仅 `matched: true` 为命中行）、`truncated`、`skipped`、`skippedLargeFiles?`、`degraded?`、`notice?`、`elapsedMs`。自动解码 UTF-16 LE/BE BOM、UTF-8 与 GB18030，跳过无 BOM 含空字节的二进制文件。
3. **遍历与预算**：两工具遵守 `.gitignore`、`.ignore`、`.fdignore`，跳过 `.git`、符号链接与未驻留云文件，最多遍历 50 万项，输出上限 50 KiB，超时 30 秒（超时返回 `isError: true`）。`ffgrep` 单文件上限 10 MiB（超限计入 `skippedLargeFiles`，改用 `read` 分段查看），单行上限 8 KiB（超限计入 `skipped`），单次总读取上限 256 MiB（达到上限保留已命中项并置 `truncated: true` 与 `degraded: true`）。
4. **零命中判读**：查找无结果时核对 `skipped`、`skippedLargeFiles` 与忽略规则；对已知具体文件可直接在 `path` 传文件路径定向检索。

## 访问拦截范围

三工具会拒读受保护凭据文件（默认位于 `data/config/{auth,models,mcp-auth,web-search}.json` 与 `data/telemetry/identity.json`）、`.state/` 目录及其硬链接或符号链接别名，同时拒绝无法核验的链接、未驻留云文件、Windows 设备路径与 NTFS 备用数据流。跨工作目录路径仍会经过路径检查，不能将工作目录视为访问隔离边界。
