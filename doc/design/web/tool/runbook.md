# runbook 工具卡片

[runbook.ts](../../../../agent/home/extensions/diagnostics/runbook.ts) 生成供人工执行的文本清单。2026-09-23 在 `E:\tmp` 的隔离副本中调用相同的注册工具与 `execute`；测试没有写入项目的 `outbox`。

## 主模板与字段组

主模板为**概览组**。成功返回 `details.runbook`，命令内容来自本次调用参数 `items[]`。

| 顺序与内容组 | 字段来源 | 展示 |
|---|---|---|
| 文件交付 · 字段组 | `runbook.file/dir/name/sequence/level/levelLabel/items/bytes/encoding` | 文件路径、序号、风险级别、条数和文本格式 |
| 命令 · 命令组 | 调用参数 `items[].shell/admin/summary/command` | 每条的执行环境、说明、原样复制命令 |
| 交付提示 | `runbook.notice` | 文件交付及执行方式 |

实测传入 `title:"工具卡片验证"`、`level:"safe"` 和一条 `cmd` 命令；返回的 `details` 节选：

```json
{"runbook":{"name":"01-安全-工具卡片验证.txt","sequence":1,"level":"safe","levelLabel":"安全","items":1,"encoding":"utf-8-bom","file":"E:\\tmp\\2026-09-23\\cst-tool-capture\\sandbox\\outbox\\2026-09-23\\01-安全-工具卡片验证.txt"}}
```

## 特殊情况

命令组是额外的交付内容组。测试文件实测含 `环境：cmd（普通权限）`、`说明：输出测试文字，不修改系统` 和 `echo cst-pilot-test`。卡片从调用参数取得命令；`runbook.items:1` 只表示条数，无法还原正文。卡片提供路径和复制按钮，不提供执行按钮。

传入 `level:"invalid"` 实测整次抛错 `level 只能是 safe / low / confirm`；传入空 `items` 实测抛错 `至少需要一条命令`。这些调用没有交付文件，主模板改为错误原文块。
