---
name: web-search
description: 联网检索与正文读取：网页搜索（web_search）、抓取正文（web_fetch）、缓存续读（get_search_content）。
---

# 联网检索（web_search / web_fetch / get_search_content）

## 检索流程与完成判据

1. 用 `web_search` 发起 1–3 个具体检索词（如错误码、蓝屏代码、硬件 ID 或型号），需要厂商文档时优先传 `includeDomains`。
2. 摘要不足以核对步骤或参数时，对高相关公开链接调用 `web_fetch` 抓取正文（单次 1–5 条）。
3. 需查看已保存正文的后续片段或关键词上下文时，拿顶层 `responseId` 调用 `get_search_content`。
4. **完成判据**：关键建议至少有可打开的公开来源链接，且从结果 URL 复核域名（`includeDomains` 在部分通道下可能被忽略或省略）；有发布日期时一并核对。若结果缺少链接或未覆盖关键步骤，继续检索或如实说明范围。

## web_search

| 参数 | 适用范围 | 约束与默认行为 |
|---|---|---|
| `query` / `queries` | 通用 | 合并共 1–3 个非空词，每词上限 4096 字节（DeepSeek 单词上限 128 字符） |
| `mode` | 通用 | `fast`（仅搜索支持）、`auto`（默认）、`deep` |
| `numResults` | 通用 | 默认 `5`，范围 `1–20`；DeepSeek `deep` 按 `max(numResults, defaultCount)` 夹在 `10–50`，实际条数可超过 20 |
| `includeDomains` / `excludeDomains` | 付费 Exa / DeepSeek | 合法域名数组（如 `["learn.microsoft.com"]`）；DeepSeek 校验域名合法性，非法值报错 |
| `afterDate` / `beforeDate` | 付费 Exa / DeepSeek | 合法 `YYYY-MM-DD` 日期；DeepSeek 校验格式，非法值报错 |
| `contentMode` | 付费 Exa / DeepSeek | `highlights`（默认）、`summary`、`text`。Exa `fast` 自动改用 `highlights`；DeepSeek `text` 或 `deep` 请求正文 |
| `provider` | 通用 | `exa` 或 `deepseek`。省略时用配置默认值；仅在配置了不同备用提供商且未显式指定 `provider` 时，主通道全败才触发回退 |
| `additionalQueries` | 付费 Exa | 最终解析为 `deep` 或 `deep-lite` 类型时生效（含 `auto` 配置到深度类型的情况），最多 10 条 |
| `languages` / `oriQuestion` | DeepSeek | `languages` 为合法语言码数组（ASCII 字母与 `-`，单码上限 16 字符，非法值报错）；`oriQuestion` 受配置 `sendOriginalQuestion` 控制，超 128 字符截断 |

返回：`responseId`、`provider`、`mode`、`queryCount`、`successfulQueries`、`queries[]`（每项含 `query`、`ok`、`resultCount`、`results[]` 内含 `title`/`url`/`snippet`/`excerpt` 及可选 `publishedAt`/`score`/`authority`/`siteName`，成功附 `preview` 与可选 `synthesis`/`grounding`，失败附 `error`）、可选 `degraded`/`degradedFrom`/`collectionErrors`/`notice`、`elapsedMs`。

提供商差异：

- **Exa 免费回退**：未配 Exa Key 且开启免费回退时走托管免费通道，忽略域名、日期与深度参数（仅用关键词、`numResults`、`fast`/`auto`），返回条目可能不含 `url`。
- **DeepSeek 长度限制**：主查询词加自动拼接的过滤算子合计限 128 字符，超限时优先保留主查询词并省略部分算子，结果在 `notice` 说明。

## web_fetch

| 参数 | 约束与默认行为 |
|---|---|
| `url` / `urls` | 合并共 1–5 个公开 HTTP/HTTPS 链接，禁止含登录凭据（userinfo） |
| `mode` | `auto`（默认，所选提供商已启用且有可用凭据时走云端抓取，否则直连并输出 `provider: "direct"`；云端失败不自动改直连）、`provider`、`direct` |
| `maxCharacters` | 返回字符上限 `500–100000`，默认 `8000` |
| `provider` | `exa` 或 `deepseek`，仅云端或 `auto` 判定时生效 |

返回：`responseId`、`provider`、`fetchMode`（`provider` / `direct`）、`urlCount`、`successful`、`urls[]`（`url`、`ok`、`title?`、`totalCharacters?`、`truncated?`、`content`、`error?`）、可选 `degraded`/`collectionErrors`、`elapsedMs`。

注意：`totalCharacters` 是本次已取得正文的长度。Exa 云端抓取可能在写入缓存前截断，单条缓存也限 2 MiB；若正文尾部未取得，改对目标页单独重抓（改 `direct` 仍须满足公开链接与网络策略），仍不全则直接说明已读范围。

## get_search_content

| 参数 | 约束与默认行为 |
|---|---|
| `responseId` | 必填，来自当前会话的 `web_search` 或 `web_fetch` |
| `findText` | 查找模式：1–4096 字节字面匹配，区分大小写，优先于 `offset` 与 `limit`；最多返回 20 处上下文、总计 6000 字符，返回 `offset` 固定为 `0` |
| `offset` / `limit` | 分段模式（省略 `findText`）：0 起字符偏移（默认 `0`）与字符数（`1–6000`，默认 `6000`） |

返回：`responseId`、`mode`（`range` / `find`）、`content`、`totalCharacters`、`offset`、`returnedCharacters`、`truncated`、`nextOffset?`、`matchCount?`（`find` 模式零命中为 `0`）、`elapsedMs`。仅能读取缓存中保存的内容；缓存查无记录可能源于 30 分钟过期、超出 32 条或 8 MiB 被驱逐，或已切换会话，此时重新检索或抓取。

## 网络限制、降级与耗时

1. 直连与云端抓取均在请求前校验公开地址与 DNS，拒绝回环地址、局域网私网与混合解析；直连抓取同时拒绝二进制媒体类型并限制正文大小，支持 UTF-8 与 GB18030 解码。
2. 单条查询或抓取失败返回 `degraded: true` 与 `collectionErrors`，已成功条目仍可用（成功请求也可能 `resultCount: 0`）；切备用提供商附 `degradedFrom`；全部失败返回 `isError: true`。
3. 单次网络请求默认超时 30 秒（`deep` 默认 60 秒，可配置，上限 180 秒）；顶层 `elapsedMs` 包含并发排队、预检与回退批次总耗时。超时或多次失败时减少并发条数或改 `web_search` 为 `fast` 重试。
