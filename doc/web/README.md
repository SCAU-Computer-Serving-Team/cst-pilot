# Web 端设计文档

规范与文件清单集中在 [DESIGN.md](../../DESIGN.md)，本目录不另存一份。冲突时以它为准。

| 文件 | 内容 |
|---|---|
| [SPEC/](SPEC/) | 实现规格 |
| [research/](research/) | 产品与技术决策、技术调研、浏览器支持 |
| [pen.md](pen.md) | Pen CLI 与 MCP 的使用注意事项 |
| [asset/](asset/) | 颜色令牌、色卡导出图与生成脚本 |

## SPEC/

| 文件 | 内容 |
|---|---|
| [dashboard.md](SPEC/dashboard.md) | 仪表盘：字段来源、费用与计时口径、Pi 接口核对 |

## research/

| 文件 | 内容 |
|---|---|
| [tech-decisions.md](research/tech-decisions.md) | 产品方向、承载与启动、技术栈、会话交互、只读边界、开发分发、架构阶段待办 |
| [tech-research.md](research/tech-research.md) | 降级能力分层、pi 多消费者、兼容版实测 |
| [browser-support.md](research/browser-support.md) | 两档内核基线、选档命令、兼容版降级、支持清单与现场策略 |

改 `DESIGN.md` 后回查本目录各文档，修掉与它矛盾的表述。
