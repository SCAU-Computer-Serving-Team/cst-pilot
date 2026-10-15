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


## 组件触发与字段来源

组件按工具名与调用参数匹配，画布标题不参与运行时匹配。「规范 · 特殊工具」集中记录共用视觉规则。每个工具的可复用组件与全部状态合并在同一个设计区，聊天示例独立保留。同一工具的风险档、权限与结果状态使用同一个组件。

| 工具与参数 | 组件 | 字段来源 |
|---|---|---|
| `bitlocker`，`scope=unlock/decrypt` | 解锁 / 解密特殊组件 | `details.bitlocker.volumes[]`、每卷 `operation`、`collectionErrors`；交互数据见下节 |
| `bitlocker`，scope 省略或 `status` | 普通工具卡片 | `details.bitlocker`；空枚举、缺失与错误分别保留 |
| `sys`，scope 省略或 `overview` | 整机概览特殊组件 | `details.overview`；内存 MB 除以 1024 展示 GB，百分比读取原字段 |
| `sys` 的其他 scope、`driver`、`eventlog` | 普通工具卡片 | `details[scope]`；`driver` 默认 `problem`，`eventlog` 默认 `recent` |
| `disk`、`startup`、`ls` | 普通工具卡片 | `details`；`startup` 使用 `details.startup` |
| `runbook`，所有风险档 | 命令清单特殊组件 | 标题、命令、说明、解释器与管理员要求来自调用参数；文件与风险档来自 `details.runbook` |
| `web_search` | 检索特殊组件 | 参数 `queries/query` 与 `details.queryCount/successfulQueries`；零来源不等于调用失败 |
| `source_check` | 普通工具卡片 | `details.artifact`、来源与引文；主张状态保持原值 |
| `fetch_content`、`get_search_content`、`ffgrep`、`fffind`、`read` | 普通工具卡片；图片单独展示 | 各工具的 `details` 与 `content`，不从文本猜造字段 |

`runbook.level=confirm` 表示人工执行前需征得机主同意。成功交付时仍显示“已生成”；后端没有清单执行、批准或已执行状态。`admin` 属于每条命令的环境要求，与风险档分别展示。诊断采集命令不进入清单，按 [runbook SKILL](../../../../agent/home/skills/runbook/SKILL.md)选择人工修复动作。

| 结果边界 | 呈现规则 |
|---|---|
| 工具尚未结束 | 从调用执行记录呈现等待 / 运行，不虚构阶段或百分比 |
| `ls` 有未统计完整项 | 保留大小下界；后端的所有 `pct` 为 `null`，不按已知大小补算 |
| BitLocker 请求接受 | `operation.status=accepted`；实际锁定与转换状态独立读取 |
| BitLocker 完全解密 | 仅 `conversionStatus=0` 确认；进度是 `collectedAt` 对应的快照，不自动修改历史调用 |
| BitLocker 未确认 | `operation.status=unknown`；只说明请求可能已提交，保持原错误与复查信息 |
| BitLocker 拒绝请求 | 展示 `operation.reason/error`；具体错误码未确认时不归因为恢复密码或自动解锁依赖 |
| 取消与等待结束 | 当前工具均可返回 `cancelled`；历史结果无法独立区分主动取消和超时；跳过项不将取消转换为成功，部分接受后取消分别保留 |
| 缺失结果元数据 | 只呈现已提供的原文并标记状态未确认，不猜造字段或打开文件入口 |
| 搜索部分失败 | 按 `successfulQueries < queryCount` 判定；所有查询失败、`details.error` 与 `isError` 均保留错误 |
| 研究结果续读 | `get_search_content` 的 research 文本为 artifact 自身的 JSON 字符片段；不增加包装或丢弃截断信息 |

## Web 交互

BitLocker 支持 TUI 与 Web。Web 的恢复密码输入、固定清单确认与期限通过会话绑定的内存请求通道提供。首屏快照与共用会话 SSE 携带待处理请求、通道身份和递增版本，不额外轮询或创建事件流。请求同时绑定助手条目与调用 ID；迟到快照不能恢复已消费请求；后端重启后按新通道身份清除原请求。提交结果未确认时先查询原请求是否仍待处理，再恢复输入或取消。

| 交互 | 接口与绑定 |
|---|---|
| 解锁输入 | 会话 / 调用 / 待处理请求标识、固定目标卷、`type=3` 的恢复保护器 ID、期限与一次性提交入口；凭据不进工具更新 |
| 解密确认 | 通过执行预检查的固定 `targets`；已锁定、已解密和正在解密的卷仍在最终结果中记录拒绝或跳过 |
| 取消 / 失效 | 当前待处理请求是否仍可提交；另一页面已处理后禁用操作，刷新不得复活确认 |
| 清单打开 | 用 `details.runbook.file/name` 对应已交付文件；详情直接显示完整 `file` 路径，不显示文本格式。本机绝对路径不能直接当 HTTP URL，需文件打开接口；不虚构工具的“已打开”结果 |

这些交互状态与工具业务结果分别关联到同一调用。运行文案读取真实工具更新阶段，不据此推断卷转换百分比。画布不通过新增工具、伪造结果字段或正文关键词触发组件。

## 实现入口

通用卡片由 `src/web/frontend/app/cards/map-tool.ts` 与 `tool-card.tsx` 转换和渲染。特殊组件由 `special-tool-data.ts` 匹配与转换，`special-tool.tsx` 独立渲染；`bitlocker-input.tsx` 只通过独立接口提交本地输入，读取状态复用会话快照与事件流。清单打开按请求去重；返回仅表示已发起记事本启动。模型文本、details 与呈现内容用途不同，禁止假设同构。
