# 项目文档

本目录按顶层总览与六个功能模块组织文档。每个模块使用同名文件区分规格、架构与接口。

## 顶层文档

| 文档 | 职责 |
|---|---|
| [SPEC.md](SPEC.md) | 产品目标、功能范围、UI 双通道要求与全局运行约束 |
| [ARCHITECTURE.md](ARCHITECTURE.md) | 系统组成、模块职责、进程与状态边界 |
| [TESTING.md](TESTING.md) | 自动检查入口、隔离原则、公共核查组与实机环境验收要求 |
| [TASKS.md](TASKS.md) | 待实现功能、待决产品议题、实机与运维待办 |
| [contract.md](contract.md) | 使用统计与遥测的字段定义、身份算法与隐私收集边界 |

根目录配套文档：[README.md](../README.md)（项目介绍与使用）、[DESIGN.md](../DESIGN.md)（Web 视觉系统与画布令牌）、[CONTRIBUTING.md](../CONTRIBUTING.md)（开发环境与提交流程）、[CHANGELOG.md](../CHANGELOG.md)（版本记录）。

## 模块文档

| 模块 | 代码位置 | 文档 |
|---|---|---|
| [runtime](runtime/SPEC.md) | `pi.cmd`、`launcher.ps1`、`agent/home/extensions/runtime/` | [SPEC](runtime/SPEC.md) · [ARCHITECTURE](runtime/ARCHITECTURE.md) |
| [diagnostics](diagnostics/SPEC.md) | `agent/home/extensions/diagnostics/`、`agent/home/skills/` | [SPEC](diagnostics/SPEC.md) · [ARCHITECTURE](diagnostics/ARCHITECTURE.md) · [API](diagnostics/API.md) |
| [web](web/SPEC.md) | `agent/home/extensions/web/`、`src/web/frontend/` | [SPEC](web/SPEC.md) · [ARCHITECTURE](web/ARCHITECTURE.md) · [API](web/API.md) |
| [auth](auth/SPEC.md) | `agent/home/extensions/oauth/`、`web/server/api/` | [SPEC](auth/SPEC.md) · [ARCHITECTURE](auth/ARCHITECTURE.md) · [API](auth/API.md) |
| [telemetry](telemetry/SPEC.md) | `agent/home/extensions/telemetry/`、`cst-pilot-server` | [SPEC](telemetry/SPEC.md) · [ARCHITECTURE](telemetry/ARCHITECTURE.md) · [API](telemetry/API.md) · [OPERATIONS](telemetry/OPERATIONS.md) |
| [release](release/SPEC.md) | `pack/`、`src/web/frontend/scripts/`、`pwsh/`、`wiztree/`、`lhm/` | [SPEC](release/SPEC.md) · [ARCHITECTURE](release/ARCHITECTURE.md) |

## Rust 重写

| 文档 | 职责 |
|---|---|
| [rust/plan/0.PLAN.md](rust/plan/0.PLAN.md) | Rust Pi 核心重写、CST Pilot 迁移与三端适配的阶段顺序、交付内容和完成条件 |
| [rust/plan/1.model-core.md](rust/plan/1.model-core.md) | 阶段 1 模型核心（`pi-ai`）的架构取舍、目录结构、核心接口与云厂商裁剪清单 |
| [rust/plan/2.agent-core.md](rust/plan/2.agent-core.md) | 阶段 2 Agent 核心（`pi-agent`）的循环、工具执行与状态管理 |
| [rust/plan/3.session-and-extensions.md](rust/plan/3.session-and-extensions.md) | 阶段 3 会话与扩展（`pi-session`）的存储、恢复与扩展接口 |
| [rust/plan/4.runtime-and-sdk.md](rust/plan/4.runtime-and-sdk.md) | 阶段 4 运行时与 SDK（`pi-runtime`）的配置、资源、工具与无头入口 |
| [rust/plan/6.cst-pilot.md](rust/plan/6.cst-pilot.md) | 阶段 6 CST Pilot 业务迁移（`cst-pilot`）的诊断工具、认证、遥测与安全边界 |
| [rust/plan/7.server.md](rust/plan/7.server.md) | 阶段 7 三端共用后台服务（`cst-server`）的进程拓扑、会话池、通道门禁与本机鉴权 |
| [rust/plan/8.web.md](rust/plan/8.web.md) | 阶段 8 Web 网页端适配的会话导出引擎、多标签聚合事件流与前后端契约对齐 |
| [rust/prototype.md](rust/prototype.md) | Rust 重写的推荐技术栈、选择理由与采用条件；尚待原型验证 |

## 同名文件职责

| 文件名 | 职责边界 |
|---|---|
| `SPEC.md` | 模块职责、功能行为、安全边界与验收条件 |
| `ARCHITECTURE.md` | 内部组成、状态归属、关键数据流与故障恢复机制 |
| `API.md` | 调用参数、HTTP/SSE 接口、返回结构、错误码与协议格式 |
| `OPERATIONS.md` | 部署参数、备份恢复、发布顺序与命令行运维操作 |
