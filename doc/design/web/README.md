# Web 实现设计

本目录定义 Web 模块、算法、存储与构建。行为和验收见 [SPEC](../../web/README.md#spec)，视觉规则见 [DESIGN.md](../../../DESIGN.md)。

| 文档 | 职责 |
|---|---|
| [frontend](frontend.md) | 源码、依赖、状态加载、静态服务与构建 |
| [runtime](runtime.md) | 会话实例、共享服务、收件箱、图片与草稿 |
| [oauth](oauth.md) | Pi 原生授权、Web 流程、授权窗与取消 |
| [fonts](fonts.md) | 字体来源、字符集、生成、缓存、回退与分发 |
| [streaming](streaming.md) | Markdown 增量分段、播放节奏与滚动 |
| [branch-tree](branch-tree.md) | 可见树几何算法与 Pi 对照 |
| [branch-summary](branch-summary.md) | 跨页待办、Pi 调用、取消与浏览器测试 |
| [tool](tool/README.md) | 工具组件、结果映射与逐工具模板 |

测试命令见[前端检查](frontend.md#检查)，执行结果与未完成范围由 MVP 和测试记录承载。
