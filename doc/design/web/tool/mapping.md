# 工具结果映射

本页定义实时和历史的结果读取。行为见[诊断卡片规格](../../../web/SPEC/diagnostic-cards.md)，通用骨架见[卡片组件](cards.md)。

## 数据来源

卡片按 `toolCallId` 关联调用参数、结果 `content`、可选 `details` 与 `isError`。实时执行和历史回放使用同一组映射规则。

卡片要覆盖当前开放的全部工具，不只是诊断工具。

| 来源 | 工具 |
|---|---|
| 诊断扩展 | `disk`、`driver`、`eventlog`、`ls`、`runbook`、`startup`、`sys` |
| pi-web-access | `web_search`、`source_check`、`fetch_content`、`get_search_content` |
| pi-fff | `ffgrep`、`fffind` |
| Pi 内置 | `read`；内置 `ls` 已被诊断扩展覆盖，其他内置工具未开放 |

| 数据 | 读取规则 |
|---|---|
| 诊断结果 | 优先读取 `details`，按工具与 scope 映射，字段语义见[工具文档](../../../tool/README.md) |
| runbook 命令组 | 环境、说明与命令来自该次调用的 `items` 参数；文件路径、序号、条数和写入结果来自 `details.runbook`。写入失败时不显示为已交付 |
| 通用工具或缺少 details | 展示 `content` 与错误状态，不假定文本必为 JSON |
| 历史回放 | 读取会话原始条目，关联调用与 `toolResult`。`buildSessionContext()` 仅保留压缩后的模型上下文 |
| 结果大小 | 诊断工具的模型 JSON 最多 50 KiB，`details` 保留完整采集结果；其他工具按自身格式处理 |

命令组表示该次调用的内容。清单后来按同一序号修正时，历史卡片不自动替换为新内容。


## 实现入口

`src/web/frontend/app/cards/map-tool.ts` 将调用、结果与状态转换为内容块；`tool-card.tsx` 使用同一套组件渲染。模型文本、details 与呈现内容用途不同，禁止假设同构。
