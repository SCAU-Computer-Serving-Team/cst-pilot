---
name: files
description: read / fffind / ffgrep 本地文件读取与检索参考。分页读文本、查看图片截图、模糊查文件路径或按正则搜日志内容时查阅。
---

# 本地文件读取与检索（read / fffind / ffgrep）

三项工具均为只读，统一受敏感路径守卫保护。

## 选择与排查路径

1. 已知文件路径，直接用 `read` 读取文本或图片。
2. 只知文件名片段或大致目录，先用 `fffind` 定位路径，再用 `read` 打开目标文件。
3. 需在多份日志或配置中查错误码与关键词，先用 `ffgrep` 定位文件与行号，再按需用 `read` 配合 `offset` 与 `limit` 读完整上下文。
4. 核对 `truncated`、`degraded`、`skipped`、`notice` 与顶层 `elapsedMs`；大文本须按 `nextOffset` 续读到获取完整证据。

## read：文本分页与图片读取

参数：`path`（必填）、`offset`（可选，1 起正整数，默认 `1`）、`limit`（可选，正整数，默认 `2000`）。

| 返回类型 | 核心字段 | 判读与续读规则 |
|---|---|---|
| 文本（`kind: "text"`） | `path`、`offset`、`limit`、`outputLines`、`totalLines`、`outputBytes`、`truncated`、`truncatedBy`（`lines` / `bytes` / `limit`）、`nextOffset?`、`firstLineExceedsLimit?`、`firstLineBytes?`、`encodingDegraded?`、`content`、`notice?`、`elapsedMs` | 0 字节文件返回 `totalLines: 0` 与空 `content`。`truncated: true` 时传 `offset: nextOffset` 续读。`firstLineExceedsLimit: true` 表示起始单行超出 50 KiB 预算，改用 `ffgrep` 检索或传 `nextOffset` 跳过该行。`encodingDegraded: true` 表示含非 UTF-8 字节并已替换解码 |
| 图片（`kind: "image"`） | `path`、`mimeType`、`bytes`、`resized`、`hints[]`、`imageAttached`、`degraded?`、`collectionErrors?`、`notice`、`elapsedMs` | 支持 PNG、JPEG、GIF、WebP、BMP，单图上限 20 MiB。正常附带图片附件块；图片损坏时降级返回单块 JSON（`imageAttached: false`、`degraded: true`）。当前模型无视觉能力时 `notice` 会提示图片已省略，严禁猜测画面内容 |

含二进制空字节或 UTF-16 编码的文本文件会直接报错拒绝（`isError: true`）；UTF-16 日志可改用 `ffgrep` 检索。

## fffind 与 ffgrep：路径查找与内容检索

| 参数 | 适用工具 | 约束与默认行为 |
|---|---|---|
| `pattern` | 两者（必填） | 上限 4096 字节。`fffind` 支持空格多词交集，传空或 `*` 列举文件；`ffgrep` 不可为空，默认按正则匹配 |
| `path` | 两者（可选） | 目录路径、文件路径或通配过滤（如 `C:/Logs/**/*.log`）；省略时从当前工作目录开始。不接受 `include` 参数 |
| `exclude` | 两者（可选） | 排除通配数组，正斜杠匹配，末尾加 `/` 排除整目录 |
| `limit` | 两者（可选） | 返回条数上限，默认 `30`，上限 `1000` |
| `caseSensitive` | 两者（可选） | 省略时含大写字母自动区分大小写 |
| `context` | 仅 `ffgrep` | 命中行前后上下文行数，默认 `0`，上限 `20` |
| `literal` | 仅 `ffgrep` | 布尔值，默认 `false`；设为 `true` 按字面子串匹配，避免正则转义问题 |

返回字段与资源上限：

1. **`fffind`**：返回 `matches[]: { path }`（精确文件名与前缀优先于分散子序列）、`truncated`、`skipped`、`notice?`（全为弱相关匹配时提示收窄条件）、`elapsedMs`。
2. **`ffgrep`**：返回 `matches[]: { path, line, text, matched }`、`truncated`、`skipped`、`skippedLargeFiles?`、`degraded?`、`notice?`、`elapsedMs`。自动识别 UTF-16 LE/BE BOM、UTF-8 与 GB18030，跳过二进制与云占位文件。
3. **`ffgrep` 护栏**：单文件上限 10 MiB（超限计入 `skippedLargeFiles`，改用 `read` 分段查看），单行上限 8 KiB，单次总读取上限 256 MiB（达到上限保留已命中项并置 `truncated: true` 与 `degraded: true`）。两工具均最多遍历 50 万项、输出上限 50 KiB、超时 30 秒。

## 安全边界与耗时判读

1. 三工具统一阻断工具包凭据（`data/config/{auth,models,mcp-auth,web-search}.json`、`data/telemetry/identity.json`）、`.state/` 目录、符号链接、目录外硬链接、Windows 设备路径与 NTFS 备用数据流。
2. 整次失败或 30 秒超时返回 `isError: true`（含 `{"error", "elapsedMs"}`）。`elapsedMs` 偏高或 `truncated: true` 时缩小 `path` 范围或补充 `exclude` 重试，禁止把截断后的零命中当作不存在。
