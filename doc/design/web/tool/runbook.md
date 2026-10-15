# runbook 工具卡片

[runbook.ts](../../../../agent/home/extensions/diagnostics/runbook.ts) 生成供人工执行的文本清单。2026-09-23 在隔离副本中调用相同的注册工具与 `execute`；测试没有写入项目的 `outbox`。

## 主模板与字段组

使用独立内嵌组件，视觉与交付动作见 [DESIGN.md](../../../../DESIGN.md#特殊工具内嵌组件)。成功返回 `details.runbook`，命令内容来自本次调用参数 `items[]`。

| 顺序与内容组 | 字段来源 | 展示 |
|---|---|---|
| 文件交付 · 字段组 | `runbook.file/name/sequence/level/levelLabel/items` | 文件名打开链接、风险档与条数；详情显示完整文件路径，不展示文本格式 |
| 命令 · 命令组 | 调用参数 `items[].shell/admin/summary/command` | 每条的执行环境、说明、原样复制命令 |
| 交付提示 | `runbook.notice` | 文件交付及执行方式 |

以下为文件交付字段示例：

```json
{"runbook":{"name":"01-安全-工具卡片验证.txt","sequence":1,"level":"safe","levelLabel":"安全","items":1,"encoding":"utf-8-bom","file":"E:\\Example\\outbox\\2026-09-23\\01-安全-工具卡片验证.txt"}}
```

## 特殊情况

卡片从调用参数取得标题与命令；`runbook.items` 只表示条数，无法还原正文。`safe/low/confirm` 共用一套组件，风险档读取 `levelLabel`；`confirm` 表示队员人工执行前需征得机主同意，清单工具没有执行确认流程。文件名链接打开已生成清单；每条命令保留复制按钮，不提供执行按钮。

清单只交付人工修复动作，诊断查询交给对应工具。行为与字段绑定见[结果映射](mapping.md#组件触发与字段来源)，文件打开接口见[Web 交互](mapping.md#web-交互)。

传入 `level:"invalid"` 实测整次抛错 `level 只能是 safe / low / confirm`；传入空 `items` 实测抛错 `至少需要一条命令`。这些调用没有交付文件，主模板改为错误原文块。
