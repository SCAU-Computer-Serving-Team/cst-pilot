# Web 端文档

本页是 Web 文档索引。[MVP](MVP.md) 定义阶段范围，[SPEC](SPEC/) 定义行为与接口草案，[research](research/) 保存依据与实测。SPEC 与详细设计的分类见[待决议题](../issues.md#d4-规格与详细设计的分类)。

视觉规范和画布资产见 [DESIGN.md](../../DESIGN.md)。产品待决事项见[待决议题](../issues.md)，工程验证与视觉待办见 [Issues](issues.md)。

## SPEC

| 文件 | 内容 |
|---|---|
| [frontend.md](SPEC/frontend.md) | 前端工程：页面形态、浏览器基线、工程边界、目录划分、加载与依赖边界、状态归属、打包接入 |
| [app-router.md](SPEC/app-router.md) | 页面地址与接口设计：请求入口、地址空间、路由组织、接口清单与约定、双版本 |
| [session-runtime.md](SPEC/session-runtime.md) | 会话运行与并行：产品要求、路由与执行分离、写权与会话串行、输入收件箱、会话可见性、共享状态、能力范围 |
| [chat-workspace.md](SPEC/chat-workspace.md) | 聊天工作台：对话流、工具调用的折叠、图片、排队与插队、中断、编辑器、状态恢复 |
| [commands.md](SPEC/commands.md) | 命令与功能对应：TUI 内置命令的 Web 呈现形式与否决清单 |
| [diagnostic-cards.md](SPEC/diagnostic-cards.md) | 诊断卡片：展示要求与约束 |
| [dashboard.md](SPEC/dashboard.md) | 仪表盘：页面结构、计数口径与呈现规则 |

## research

| 文件 | 内容 |
|---|---|
| [tech-decisions.md](research/tech-decisions.md) | 承载与启动、页面形态、前端技术栈、开发与分发 |
| [pi-capability.md](research/pi-capability.md) | Pi 0.85.1 能力清单、内置命令与可用 API、扩展运行约定、发行二进制、运行承载、用量与费用字段 |
| [measurements.md](research/measurements.md) | 四轮实测的数字：文件数与解压、形态对比、内核能力、框架表现、Next 版本边界 |
| [opencode.md](research/opencode.md) | 参考实现的形态对照、收件箱分层、可借鉴与不适合照搬的做法 |

## Issues

| 文件 | 内容 |
|---|---|
| [../issues.md](../issues.md) | 需要产品或维护者确认的选择 |
| [issues.md](issues.md) | 工程验证与视觉设计待办 |

## 其他

| 文件 | 内容 |
|---|---|
| [MVP.md](MVP.md) | MVP 范围与验收、后续发行接入 |
| [pen.md](pen.md) | Pen CLI 与 MCP 的使用注意事项 |
| [../design/web/tool/](../design/web/tool/) | 工具卡片：通用组件、逐工具返回形态与画布选择 |
| [asset/](asset/) | 颜色令牌、色卡导出图与生成脚本 |

修改行为、接口或视觉规则后，回查引用该规则的文档，避免重复定义与冲突。
