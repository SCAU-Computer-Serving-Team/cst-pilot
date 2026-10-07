# CST Pilot

<p align="center"><img src="assets/logo.png" alt="CST Pilot" width="820"></p>

CST Pilot 是计算机维护队的便携诊断 Agent，基于 [Pi](https://github.com/earendil-works/pi)。默认开放读取、检索和诊断工具，修复命令由队员确认后手动执行。

**Web 端为实验性质。**

[发行下载](https://github.com/SCAU-Computer-Serving-Team/cst-pilot/releases) · [更新记录](CHANGELOG.md) · [产品需求](doc/PRD.md) · [开发规范](CONTRIBUTING.md) · [文档索引](doc/README.md)

## 能力

| 能力 | 内容 |
|---|---|
| 磁盘与目录 | 空间、健康、文件占用、目录排行；WizTree 加速扫描 |
| 系统状态 | 进程、CPU、内存、GPU、传感器与整机负载 |
| 启动与故障 | 自启、设备与驱动、Windows 事件日志 |
| 命令交付 | 在工具包 `outbox/` 生成 txt 清单，按风险分档，供队员逐条执行 |
| TUI | `pi.cmd` 启动命令行界面 |
| Web（实验性质） | `/web` 接管当前会话；支持历史、分支树、模型、授权、图片与队列 |

配置、凭据与会话共用 `agent/home`。同一会话只允许一个运行实例；TUI 和 Web 的主题独立保存。诊断返回的降级、权限与准确性限制见[工具文档](doc/tool/README.md)。

## 使用

1. 完整解压发行 ZIP，运行 `pi.cmd`。
2. 首次使用执行 `/login`，选择模型服务并登录。发行包不提供密钥。
3. 使用 TUI，或执行 `/web` 在本机浏览器继续操作。接管后原终端待机；关闭终端会结束服务。
4. Web 退出使用账号菜单的“退出 CST Pilot”。“退出 CSTOA”只退出账号。

发行包内置 Pi、PowerShell 和必要诊断程序，现场无需安装 Node、Python 或 npm。默认发行版本为 `v0.5`。当前测试结果与未验证范围见[联调报告](doc/test/checkpoint5-report.md)和[跨端报告](doc/test/shared-backend-report.md)，目标设备与完整Checkpoint5验收仍未完成。

## 目录

| 路径 | 职责 |
|---|---|
| `pi.cmd` | 隔离启动器 |
| `agent/home/extensions/` | 品牌、诊断、认证、运行协调、Web 与遥测 |
| `agent/home/skills/` | 诊断工具说明 |
| `src/web/frontend/` | Web 源码与构建工具 |
| `src/web/design/`、`DESIGN.md` | 画布、视觉资产与设计规范 |
| `doc/` | 需求、规格、设计、工具、认证、遥测与测试文档 |
| `pack/` | 官方运行时与白名单装配、校验、发行冒烟 |

开发仓库不包含完整便携运行时。开发依赖与现场运行链分开，准备方法见 [CONTRIBUTING.md](CONTRIBUTING.md)。

发行目录包含 `agent/.runtime/`、预构建 Web、必要扩展与原生依赖、`pwsh/`、`wiztree/`、`lhm/`、许可、`VERSION`、`BUILD-INFO.json` 和 `SHA256SUMS`。开发 `AGENTS.md`、源码测试、机主数据、密钥和会话不进入发行包；运行指令由 `agent/home/APPEND_SYSTEM.md` 提供。

## 边界与许可

- 配置、凭据与会话写入工具包，临时状态在 `.state/`。Windows 日志与原生缓存的边界见[运行注意事项](doc/Notice.md)。
- 模型服务与联网工具按配置访问网络。网页抓取仅放行 TUN 假 IP 段 `198.18.0.0/15`，其他私网与本机回环仍被拒绝。
- 项目遥测默认启用，上传需有效 CSTOA 登录；可在 `agent/home/telemetry.json` 将 `enabled` 设为 `false`。生产授权与接收端部署仍需验收。
- WizTree 个人使用免费，商业使用需授权。本项目以 [MIT](LICENSE) 分发；第三方条款见 [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)。
