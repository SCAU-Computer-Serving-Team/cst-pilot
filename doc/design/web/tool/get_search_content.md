# get_search_content 工具卡片

项目安装的 `pi-web-access@0.18.0` 使用 `responseId` 读取此前 `web_search`、`source_check`、`fetch_content` 的储存结果。2026-09-23 将三个工具的真实 `responseId` 依次传入项目扩展；ID 为本次调用临时生成，不写入文档。

## 主模板与字段组

主模板为**原文块**。不同来源的续读字段不同，不把正文解析成统一的排行。

| 来源 | 字段组 | 原文块 |
|---|---|---|
| 搜索结果 | `details.query/resultCount` | `content` 中的该查询答案与来源 |
| 研究结果 | `details.type/contentLength/offset/returnedChars/nextOffset/truncated` | `artifact` 的 JSON 字符片段 |
| 网页抓取 | `details.url/title/contentLength/offset/nextOffset/truncated` | 页面内容片段；`findText` 时显示命中上下文 |

## 各来源的实测返回

- `web_search` 的 `searchId` + `queryIndex:0`：`details.resultCount:1`，正文以 `## Results for: "IANA reserved example domains"` 开头。
- `source_check` 的 `responseId` + `offset:0,limit:180`：`details:{"type":"research","offset":0,"returnedChars":180,"nextOffset":180,"truncated":true}`，正文为 JSON 片段。
- `fetch_content` 的 `responseId` + `urlIndex:0,offset:0,limit:180`：正文为网页前 180 个字符，`nextOffset:180`；改用 `findText:"example.com"`，正文给出匹配段落，并附 `findMode/matchCount/returnedMatches/queryResults`。

## 特殊情况

输入不存在的 `responseId:"absent-123"` 实测返回 `content:"Error: No stored results for \"absent-123\""`、`details:{"error":"Not found","responseId":"absent-123"}`。此时主模板为错误原文块。项目安装版本对研究结果按 `offset/limit` 切分 JSON；不能把 JSON 字符片段当作完整研究结论。`resultCount` 是该搜索查询的来源数，不能当成本次正文条数。
