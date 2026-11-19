# Rust 工作区子包定位

本页记录 `crates/` 工作区各包的编译产物、所属进程与修改入口。系统拓扑与进程关系见 [ARCHITECTURE.md](../ARCHITECTURE.md)，阶段路线见 [0.PLAN.md](plan/0.PLAN.md)。

## 命名约定

- `pi-*`：对齐上游 Pi `1.5.0` 的通用核心层，不放 CST Pilot 业务规则。
- `cst-*`：CST Pilot 专用层。`cst-server`、`cst-updater`、`cst-desktop` 产出可执行文件，其余为库包。

## 子包速查表

| 包目录 | 编译产物 | 所属进程 | 常见修改入口 |
|---|---|---|---|
| `crates/pi-ai` | 库 | 后台服务（经 `cst-server`） | 模型协议适配、流式事件、内置模型目录 |
| `crates/pi-agent` | 库 | 后台服务（经 `cst-server`） | Agent 循环、工具参数校验、并发工具调度 |
| `crates/pi-session` | 库 | 后台服务（经 `cst-server`） | JSONL v3 会话树、压缩、静态扩展接口、恢复日志 |
| `crates/pi-runtime` | 库 + `pi` CLI | 后台服务（库）/ 上游 CLI 验证 | 设置与模型管理、资源加载、SDK、`codemode`/`mcp` 可选编译开关 |
| `crates/cst-os` | 库 | 后台服务、更新器、软件端共用 | 根目录解析、`.state/server.json` 发现记录、`ProcessIdentity`、安全路径与内核锁、JSON 辅助 |
| `crates/cst-updater` | 库 + `cst-updater.exe` | 独立更新器进程 / 离线验签库 | Ed25519 验签与归档验树、升级布局白名单、事务替换与中断恢复、`cst-updater.exe` |
| `crates/cst-kit` | 库 | 后台服务（经 `cst-server`） | 17 个诊断工具、BitLocker、Runbook、CSTOA 认证、双端遥测、联网升级检查与拉起编排 |
| `crates/cst-server` | 库 + `cst-server.exe` | 公用后台服务进程 | 服务发现、通道门禁、HTTP/SSE 路由、会话池、收件箱、会话导出 |
| `crates/cst-client`（阶段 9 规划） | 库 | 软件端进程 | HTTP/SSE 强类型客户端与本地只读缓存 |
| `crates/cst-desktop`（阶段 9 规划） | `cst-pilot.exe` | 软件端进程 | GPUI 桌面界面、D3D11 背景与窗口控制 |
| `crates/cst-tui`（阶段 10 规划） | 库 | 软件端进程 | 桌面窗口内置 TUI 视图 |
