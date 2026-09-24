# ls 工具卡片

[ls.ts](../../../../agent/home/extensions/diagnostics/ls.ts) 覆盖 Pi 内置 `ls`。输入 `path` 和可选 `top`；返回 `details` 与内容相同的诊断 JSON。以下为 2026-09-23 直接调用项目工具的本机结果节选。

## 主模板与字段组

**排行组**为主模板。`entries[]` 已按 `bytes` 降序排列；只包含目录的直接子项。

| 顺序与内容组 | 原始字段 | 展示 |
|---|---|---|
| 目录概况 · 字段组 | `path`、`totalChildren`、`totalSize`、`method` | 当前目录、直接子项总数、总大小与统计方式 |
| 子项大小 · 排行组 | `entries[].name/type/size/bytes/pct` | 文件或目录、大小、占比；长名称允许折行 |
| 省略项 · 提示 | `omitted.count/size/unknownCount/note` | 有截断时显示剩余项数及体积 |

`path` 指向 `E:\Learning\Programming\cst-pilot\doc\design\web\tool`，`top:1` 的实测返回：

```json
{
  "totalChildren": 16,
  "totalSize": "45.8 KB",
  "method": "wiztree-index",
  "entries": [{"name":"cards.md","type":"file","size":"10.9 KB","bytes":11195,"pct":23.9}],
  "omitted": {"count":15,"unknownCount":0,"size":"34.8 KB","note":"已按大小截断，其余为小项"}
}
```

## 特殊情况

空目录实测返回 `{"totalChildren":0,"totalSize":"0 B","entries":[]}`。保留目录概况，排行组显示“目录为空”；空数组表示本次列目录成功。

不存在的路径实测整次抛错：`路径不存在: E:\tmp\2026-09-23\cst-tool-capture\not-a-directory`。主模板改为原文块，不显示成功排行。`size/bytes/pct` 为 `null` 或出现 `notice` 时按字段值说明统计不完整；该分支由代码确认，尚未实测。
