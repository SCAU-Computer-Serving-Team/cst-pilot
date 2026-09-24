# read 工具卡片

Pi 内置 `read` 输入 `path`、可选 `offset` 与 `limit`，返回 `content` 数组。以下样本直接调用项目安装的 Pi 实现，日期为 2026-09-23。

## 主模板与字段组

**原文块**为主模板。文本读取只有一个字段组：绝对路径（`path` 原样显示，不附 offset/limit）；正文保留换行和续读提示。正常读取的 `details` 可以缺席；不从文件正文推造结构化字段。

| 内容组 | 原始字段 | 展示 |
|---|---|---|
| 绝对路径 · 字段组 | 调用参数 `path` | 绝对路径一行 |
| 文件正文 · 原文块 | `content[]` 的 `type:"text"` | 保留换行和续读提示 |

读取图片是例外，见[例外](#例外)。

用临时文件 `alpha\nbeta\ngamma`、`limit:2` 实测，返回的 `details` 缺席：

```json
{"content":[{"type":"text","text":"alpha\nbeta\n\n[1 more lines in file. Use offset=3 to continue.]"}]}
```

## 例外

读取图片不套用上表：先展示图片本身，再展示绝对路径一行。「Read image file [image/png]」只是工具输出里的标注块，不绘制成内容行。画布：「工具调用 · read · 例外 · 图片 浅色」。

一次调用只读一个文件，不会同时返回文本正文与图片。图片场景返回的 text 块是标注（`Read image file [...]`，处理失败时附带错误说明），BMP 等不支持格式只有标注没有图块；文本场景只有正文块。

## 特殊情况

读取临时 PNG 实测依次返回 `{"type":"text","text":"Read image file [image/png]"}` 与 `{"type":"image","mimeType":"image/png"}`。实际图片块另含长度 208 的 `data` 字符串；示例只列用于选组的字段。必须显示图片组。图片处理失败时按原文展示说明；该失败分支由代码确认，尚未实测。

用 2200 行临时文本实测自动截断：`details.truncation.truncated:true`、`truncatedBy:"lines"`、`totalLines:2200`、`outputLines:2000`，正文尾部提示下一次 `offset=2001`。与上方按 `limit` 提前停止的结果不同，后者没有 `details.truncation`。不存在的文件和越界 `offset` 均实测整次抛错，主模板改为错误原文块。
