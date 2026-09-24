# ffgrep 工具卡片

项目安装的 `@ff-labs/pi-fff@0.10.6` 将文件名、行号、匹配行和上下文写入 `content` 文本。以下调用均使用项目安装的扩展，日期为 2026-09-23。

## 主模板与字段组

**原文块**为主模板：保留文件分组、命中行、上下文和游标。`details` 没有匹配条目数组。

| 内容组 | 原始字段 | 展示 |
|---|---|---|
| 匹配统计 · 字段组 | `details.totalMatched`、`details.totalFiles` | 当前页命中条数、索引文件数；`totalFiles` 不是命中文件数 |
| 文件匹配 · 原文块 | `content[].text` | 按原文顺序展示文件名、行号、上下文及续页提示 |

搜索 `diagnosticResult`，`path:"agent/home/extensions/diagnostics/"`、`limit:1`，实际返回：

```text
agent/home/extensions/diagnostics/driver.ts
 5: import { diagnosticResult, OUTPUT_GUIDELINE, throwOnError } from "./result.ts";
 64: return diagnosticResult(result);

[Continue with cursor="fff_c1"]
```

同次返回 `details:{"totalMatched":2,"totalFiles":162}`。传入 `cursor:"fff_c1"` 实测下一页为 `eventlog.ts` 的 2 条匹配，游标变为 `fff_c2`。引擎会先输出完当前文件，所以 `limit:1` 仍返回 2 条。

## 特殊情况

零匹配：`content` 为 `No matches found`，`details` 为 `{"totalMatched":0,"totalFiles":162}`。通配符 `pattern:".*"` 实测返回引导说明文本，`details` 为 `{"totalMatched":0,"totalFiles":0}`。两者都没有 `isError`；卡片分别显示“无匹配”与“请缩小查询”，不可只看数字零。正则 `diagnostic(Result|Command)` 实测保留相同的文件分组和游标格式。
