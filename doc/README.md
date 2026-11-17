# 项目文档

本目录按顶层总览、八个功能模块与 Rust 子包设计组织文档。各模块通过同名文件划分规格、架构、接口与运维职责。

## 顶层文档

| 文档 | 职责 |
|---|---|
| [SPEC.md](SPEC.md) | 产品定位、诊断与受控操作需求、软件内升级要求、三端通道与全局约束 |
| [ARCHITECTURE.md](ARCHITECTURE.md) | 双进程三端拓扑、八大模块边界、存储状态归属与安全隔离策略 |
| [TESTING.md](TESTING.md) | 自动化检查命令、隔离原则、诊断公共核查组（B01–B35）与实机验收矩阵 |
| [TASKS.md](TASKS.md) | 主线阶段待办、功能迭代、设计规范重构、分类待决议题与实机生产验收清单 |
| [contract.md](contract.md) | 使用情况遥测的字段定义、匿名身份算法与隐私不采集边界 |

根目录配套文档涵盖项目介绍与使用（[README.md](../README.md)）、视觉系统规范（[DESIGN.md](../DESIGN.md)），以及开发提交流程（[CONTRIBUTING.md](../CONTRIBUTING.md)）与版本记录（[CHANGELOG.md](../CHANGELOG.md)）。

## 功能模块

| 模块 | 代码与资源位置 | 模块文档 |
|---|---|---|
| [pi-core](pi-core/SPEC.md) | `crates/pi-{ai,agent,session,runtime}` | [SPEC](pi-core/SPEC.md) · [ARCHITECTURE](pi-core/ARCHITECTURE.md) · [API](pi-core/API.md) |
| [server](server/SPEC.md) | `crates/cst-server`、`pi-web-rust.cmd` | [SPEC](server/SPEC.md) · [ARCHITECTURE](server/ARCHITECTURE.md) · [API](server/API.md) |
| [diagnostics](diagnostics/SPEC.md) | `crates/cst-pilot/src/{kit,tools,diagnostics,runbook,bitlocker}`、`agent/home/skills/`、`wiztree/`、`lhm/` | [SPEC](diagnostics/SPEC.md) · [ARCHITECTURE](diagnostics/ARCHITECTURE.md) · [API](diagnostics/API.md) |
| [auth](auth/SPEC.md) | `crates/cst-pilot/src/auth/`、`crates/cst-server/src/routes/{auth,models}.rs` | [SPEC](auth/SPEC.md) · [ARCHITECTURE](auth/ARCHITECTURE.md) · [API](auth/API.md) |
| [telemetry](telemetry/SPEC.md) | `crates/cst-pilot/src/telemetry/`、`cst-pilot-server` | [SPEC](telemetry/SPEC.md) · [ARCHITECTURE](telemetry/ARCHITECTURE.md) · [API](telemetry/API.md) · [OPERATIONS](telemetry/OPERATIONS.md) |
| [web](web/SPEC.md) | `src/web/frontend/`、`crates/cst-server/src/exports/`、`design/cst-pilot-web.pen` | [SPEC](web/SPEC.md) · [ARCHITECTURE](web/ARCHITECTURE.md) · [API](web/API.md) · [路由广度 E2E](web/test/bsk-e2e.md) · [深度状态机 E2E](web/test/bsk-deep-e2e.md) · [UX 量化 E2E](web/test/ux-e2e.md) |
| [desktop](desktop/SPEC.md) | `design/cst-pilot-desktop.pen`、规划 `crates/{cst-client,cst-desktop,cst-tui}` | [SPEC](desktop/SPEC.md) · [ARCHITECTURE](desktop/ARCHITECTURE.md) |
| [release](release/SPEC.md) | `crates/cst-pilot/src/update/`、`src/web/frontend/scripts/`、`pack/` | [SPEC](release/SPEC.md) · [ARCHITECTURE](release/ARCHITECTURE.md) |

## Rust 子包设计与路线图

| 文档 | 职责 |
|---|---|
| [rust/prototype.md](rust/prototype.md) | Rust 核心服务、三端界面与 Windows 原生诊断的技术栈选型及依据 |
| [rust/plan/0.PLAN.md](rust/plan/0.PLAN.md) | 阶段 0–11 路线图、模块分层边界、完成状态与已拍板工程决策 |
| [rust/plan/1.model-core.md](rust/plan/1.model-core.md) | `crates/pi-ai` 多协议流式客户端、模型目录与云厂商裁剪清单 |
| [rust/plan/2.agent-core.md](rust/plan/2.agent-core.md) | `crates/pi-agent` 双层循环、三阶段参数转换校验与并发工具控制 |
| [rust/plan/3.session-and-extensions.md](rust/plan/3.session-and-extensions.md) | `crates/pi-session` JSONL v3 会话树、压缩、静态扩展与中断恢复日志 |
| [rust/plan/4.runtime-and-sdk.md](rust/plan/4.runtime-and-sdk.md) | `crates/pi-runtime` 配置服务、资源加载、MCP/Codemode 与公开 SDK |
| [rust/plan/6.cst-pilot.md](rust/plan/6.cst-pilot.md) | `crates/cst-pilot`（阶段 9 更名 `cst-kit`）原生诊断、认证、遥测、BitLocker 与升级设计 |
| [rust/plan/7.server.md](rust/plan/7.server.md) | `crates/cst-server` 双状态机门禁、会话池、收件箱与多会话聚合 SSE 设计 |
| [rust/plan/8.web.md](rust/plan/8.web.md) | Web 端 `SharedWorker` 聚合流接入与 `Chromiumoxide` 会话导出引擎设计 |
| [rust/plan/9.desktop.md](rust/plan/9.desktop.md) | 阶段 9 `crates/cst-client` 与 `crates/cst-desktop` GPUI 桌面端详细设计 |

## 同名文件职责

| 文件名 | 职责边界 |
|---|---|
| `SPEC.md` | 模块职责、功能行为、安全边界与验收条件 |
| `ARCHITECTURE.md` | 内部组成、状态归属、关键数据流与故障恢复机制 |
| `API.md` | 调用参数、HTTP/SSE 协议、返回结构、错误码与数据格式 |
| `OPERATIONS.md` | 部署参数、备份保留期、发布顺序与命令行运维操作 |
