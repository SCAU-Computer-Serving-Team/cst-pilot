# Web 文档

| 范围 | 职责 |
|---|---|
| [MVP](MVP.md) | 阶段范围与验收状态 |
| [SPEC](SPEC/) | 当前行为、接口、约束与验收标准 |
| [实现设计](../design/web/README.md) | 模块、算法、存储、接线、构建与测试入口 |
| [research](research/) | 调研依据与实测 |
| [DESIGN.md](../../DESIGN.md) | 视觉规则与画布资产 |
| [议题](../issues.md)、[Todo](../Todo.md) | 未决选择、未完成验证与待办 |

规则只在所属文档定义，其他文档用链接引用。已完成阶段不在每份规格重复记状态。

## SPEC

| 文件 | 内容 |
|---|---|
| [frontend](SPEC/frontend.md) | 浏览器、导航、状态归属、输入与界面约束 |
| [app-router](SPEC/app-router.md) | 页面地址、接口与安全契约 |
| [auth](SPEC/auth.md) | 模型服务、OAuth、API KEY、专属账号与启用模型 |
| [session-runtime](SPEC/session-runtime.md) | 接管、写权、并行、输入队列与共享边界 |
| [chat-workspace](SPEC/chat-workspace.md) | 消息、过程折叠、图片、队列、编辑器与恢复 |
| [branch-tree](SPEC/branch-tree.md) | 可见树、几何语义、导航与折叠 |
| [branch-summary](SPEC/branch-summary.md) | 三种选择、待办、结果与取消 |
| [commands](SPEC/commands.md) | 命令映射、范围与快捷键 |
| [diagnostic-cards](SPEC/diagnostic-cards.md) | 14 个工具的状态与展示要求 |
| [dashboard](SPEC/dashboard.md) | MVP 后的用量页面与计数口径 |

## 调研与资源

| 文件 | 内容 |
|---|---|
| [tech-decisions](research/tech-decisions.md) | 承载、形态、技术栈与分发选择理由 |
| [pi-capability](research/pi-capability.md) | 锁定 Pi 的 API、命令、运行承载与用量边界 |
| [measurements](research/measurements.md) | 文件、解压、框架与内核实测 |
| [opencode](research/opencode.md) | 参考实现的组织与可借鉴内容 |
| [pen](pen.md) | Pen CLI 与 MCP 操作注意事项 |
| [asset](asset/) | 色表与生成脚本 |
