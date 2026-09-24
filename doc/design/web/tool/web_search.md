# web_search 工具卡片

项目安装的 `pi-web-access@0.18.0` 返回 `content` 文本与 `details` 摘要。2026-09-23 通过项目扩展直接调用 `execute`，选择 `workflow:"none"`、`provider:"exa"`；正文只摘录公开页面内容。

## 主模板：例外

web_search 不套用三主模板，是[例外画布](../../../web/SPEC/diagnostic-cards.md#渲染规则)：不进入折叠，单独占一行，按[输出正文第 1 层](../../../../DESIGN.md#工具调用卡片)以 16px 行内呈现，与聊天工作台的联网检索行同构。运行中为地球图标 + 「正在联网检索…」+ 加载圆弧；展开后仅显示传入的关键词，12px；不弹出工具返回内容，完成以图标标识。返回数据仍按下方结构进入原始数据。

| 状态 | 行内容 | 字号 |
|---|---|---|
| 运行中 | 地球图标 + 「正在联网检索…」+ 加载圆弧 | 16px |
| 展开 | 传入的关键词（queries 以 · 连接） | 12px |
| 已完成 | 加载圆弧换为成功图标，文案改「已联网检索」 | 16px |
| 失败 | 错误图标，文案改「联网检索失败」，关键词行显示错误文本 | 16px / 12px |

单查询 `IANA reserved example domain` 实测 `{"queryCount":1,"successfulQueries":1,"totalResults":2}`；双查询输入 `queries:["IANA reserved example domains","RFC 2606 example.com"]` 实测：

```json
{"details":{"queries":["IANA reserved example domains","RFC 2606 example.com"],"queryCount":2,"successfulQueries":2,"totalResults":2,"includeContent":false,"fetchId":null,"searchId":"mudxf0ae34g9jm"}}
```

双查询正文按 `## Query: "..."` 分节，随后是结果与来源。正文有来源链接，`details` 不提供普通搜索的来源条目数组。

## 特殊情况

省略 `query/queries` 实测返回 `content:"Error: No query provided. Use 'query' or 'queries' parameter."`、`details:{"error":"No query provided"}`，没有 `isError`。使用错误原文块。部分查询失败时仍可能保留其他查询的结果；状态须比较 `successfulQueries` 与 `queryCount`。人工筛选流程的 `curatedQueries[]` 和后台抓取 `fetchId` 属于代码可知分支，本次未实测，不从普通返回推造这些字段。
