# fffind 工具卡片

项目安装的 `@ff-labs/pi-fff@0.10.6` 依相关度返回路径文本。以下调用均使用项目安装的扩展，日期为 2026-09-23。

## 主模板与字段组

**原文块**为主模板。`details` 仅有统计和分页字段，没有路径数组；路径顺序以 `content` 为准。

| 内容组 | 原始字段 | 展示 |
|---|---|---|
| 查找统计 · 字段组 | `details.totalMatched/totalFiles/pageIndex/hasMore` | 匹配总数、索引文件数、当前页及是否有下一页 |
| 路径结果 · 原文块 | `content[].text` | 路径、文件标注与下一页游标；保持原有顺序 |

搜索 `disk`，限定 `path:"doc/design/web/tool/**"`、`limit:2` 的实测首屏：

```text
doc/design/web/tool/disk.md  [untracked in git]
doc/design/web/tool/source_check.md  [untracked in git]

[14 more matches available. cursor="1" to continue]
```

对应 `details:{"totalMatched":16,"totalFiles":162,"pageIndex":0,"hasMore":true}`。传入 `cursor:"1"` 实测下一页 `pageIndex:1`、`hasMore:true`，文本继续输出两条路径。

## 特殊情况

查询不存在的 `unfindable-xyz-2099` 实测返回 `content:"No files found matching pattern"`，`details:{"totalMatched":0,"totalFiles":162,"pageIndex":0,"hasMore":false}`。显示空结果，不绘制空排行，也不把 `totalFiles` 当作命中数量。
