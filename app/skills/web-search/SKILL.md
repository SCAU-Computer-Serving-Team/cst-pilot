---
name: web-search
description: web_search / web_fetch / get_search_content 联网检索与正文续读参考。查故障文档、驱动型号、错误码或读取长网页正文时查阅。
---

# 联网检索（web_search / web_fetch / get_search_content）

三项只读联网工具分工协作：先检索定位来源，再按需抓正文，正文超长时从会话内存缓存续读。

## 检索流程

1. 调用 `web_search` 发起 1–3 个精准关键词并发检索。核查蓝屏代码、错误码、硬件 ID 或官方方案时，优先用 `includeDomains` 限定厂商或文档站点。
2. 返回摘要不足以确认步骤或参数时，挑高相关链接调 `web_fetch` 批量抓取正文（单次 1–5 个链接）。
3. 摘要或正文被截断时，拿顶层 `responseId` 调 `get_search_content` 分页续读，或传 `findText` 直接定位关键片段。
4. **完成判据**：核对来源域名、发布时间、`degraded`、`degradedFrom`、`collectionErrors` 与 `elapsedMs`，只把有明确来源支撑的结论或命令交给队员。

## 工具参数与返回

### web_search

| 参数 | 约束与默认行为 |
|---|---|
| `query` / `queries` | 合并共 1–3 个非空词，每词上限 4096 字节（DeepSeek 单词上限 128 字符） |
| `mode` | `fast`（快速核实）、`auto`（默认）、`deep`（深度检索，超时更长） |
| `numResults` | 单查询条数，1–20（DeepSeek `deep` 下限升至 10） |
| `includeDomains` / `excludeDomains` | 域名过滤数组（如 `["learn.microsoft.com"]`） |
| `afterDate` / `beforeDate` | 发布日期区间，`YYYY-MM-DD` |
| `languages` | 语言码数组（如 `["zh", "en"]`，作用于 DeepSeek） |
| `contentMode` | `highlights`（默认）、`summary`、`text`（Exa `fast` 自动将 `summary` 折回 `highlights`） |
| `provider` | `exa` 或 `deepseek`；省略时使用配置默认提供商并在全败时尝试备用回退，显式传入时不触发回退 |
| `additionalQueries` | 仅 Exa `deep` 生效，最多 10 个扩展检索词 |
| `oriQuestion` | 仅 DeepSeek 生效，原始用户问题语境（超 128 字符自动截断） |

返回：`responseId`、`provider`、`mode`、`queryCount`、`successfulQueries`、`queries[]`（每项含 `query`、`ok`、`resultCount`、`results[]` 内含 `title`/`url`/`snippet`/`excerpt` 及可选 `publishedAt`/`score`/`authority`/`siteName`，成功附 `preview` 与可选 `synthesis`/`grounding`，失败附 `error`）、可选 `degraded`/`degradedFrom`/`collectionErrors`/`notice`、`elapsedMs`。

### web_fetch

| 参数 | 约束与默认行为 |
|---|---|
| `url` / `urls` | 合并共 1–5 个公开 HTTP/HTTPS 链接 |
| `mode` | `auto`（默认，有提供商 Key 走云端清洗，否则直连）、`provider`、`direct` |
| `maxCharacters` | 单页返回字符上限，500–100000，默认 `8000` |
| `provider` | `exa` 或 `deepseek`，仅 `provider` 或 `auto` 模式生效 |

返回：`responseId`、`provider`、`fetchMode`（`provider` / `direct`）、`urlCount`、`successful`、`urls[]`（`url`、`ok`、`title?`、`totalCharacters?`、`truncated?`、`content`、`error?`）、可选 `degraded`/`collectionErrors`、`elapsedMs`。

### get_search_content

| 参数 | 约束与默认行为 |
|---|---|
| `responseId` | 必填，来自当前会话的 `web_search` 或 `web_fetch` |
| `offset` / `limit` | 分段模式：0 起字符偏移（默认 `0`）与字符数（1–6000，默认 `6000`） |
| `findText` | 查找模式：1–4096 字节字面匹配，最多返回 20 处上下文、总计 6000 字符 |

返回：`responseId`、`mode`（`range` / `find`）、`content`、`totalCharacters`、`offset`、`returnedCharacters`、`truncated`、`nextOffset?`（`range` 截断时）、`matchCount?`（`find` 模式，零命中为 `0`）、`elapsedMs`。

## 提供商边界、缓存与降级判读

1. **Exa 免费回退**：未配 Exa Key 时自动走托管免费通道，仅生效关键词、条数与 `fast`/`auto` 模式；配置 Key 后完整支持域名、日期、`deep` 与 `additionalQueries`。
2. **DeepSeek 长度守卫**：主查询词与自动拼接的过滤算子合计上限 128 字符，超限时优先保留主查询词并省略部分过滤算子，结果在 `notice` 说明。
3. **缓存生命周期**：`responseId` 仅驻留当前会话内存 30 分钟（上限 32 条、总计 8 MiB、单条 2 MiB）；过期报错时重新检索。
4. **网络安全**：直连与云端抓取均拒绝回环地址、局域网私网与混合 DNS 解析；直连抓取拒绝二进制媒体类型，支持 UTF-8 与 GB18030 解码。
5. **降级与耗时**：多词或多链接部分失败返回 `degraded: true` 与 `collectionErrors`，已成功条目仍可使用；自动切备用提供商时附 `degradedFrom`；全部失败返回 `isError: true`。`elapsedMs` 偏高说明网络受阻或目标站点响应慢，可降级为 `fast` 或减少并发数。
