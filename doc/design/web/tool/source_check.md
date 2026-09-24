# source_check 工具卡片

项目安装的 `pi-web-access@0.18.0` 返回 `details.artifact` 和 `responseId`。2026-09-23 直接调用项目扩展，以 IANA 示例域名为公开测试材料。

## 主模板与字段组

主模板为**概览组**，先呈现主张判断，再展示证据。

| 内容组 | 数据路径 | 展示 |
|---|---|---|
| 主张判断 · 字段组 | `artifact.query`、`claims[].status/confidence/rationale` | 状态、可信度和理由；缺乏证据时保留原状态 |
| 来源 · 排行组 | `artifact.sources[].rank/title/url/quality/fetched` | 来源顺序与是否取得正文 |
| 引文 · 原文块 | `artifact.passages[].text/source_url/source_rank` | 原文及来源关联 |
| 继续查看 · 字段组 | `details.responseId/sourceCount/passageCount` | 原始研究结果标识与实际条数 |

输入主张 `example.com is reserved for documentation`，`fetchContent:false` 实测 2 个来源、0 条引文，结果节选：

```json
{"sourceCount":2,"passageCount":0,"artifact":{"claims":[{"status":"missing-evidence","confidence":0.2}],"sources":[{"rank":1,"fetched":false}],"passages":[]}}
```

## 特殊情况

相同主张改为 `fetchContent:true`、限定 `iana.org`，实测 1 个来源、3 条引文，但判断仍是 `unclear`：

```json
{"sourceCount":1,"passageCount":3,"artifact":{"claims":[{"status":"unclear","confidence":0.3}],"sources":[{"rank":1,"title":"RFC 6761: Special-Use Domain Names","url":"https://www.iana.org/go/rfc6761"}]}}
```

来源数和引文数不能替代 `claims[].status`。缺少主张实测返回 `details:{"error":"Missing claim"}` 和错误文本，使用原文块。来源抓取失败时检查 `artifact.errors[]` 和来源级错误，保留已取得的证据。
