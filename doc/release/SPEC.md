# 便携发行与软件升级规格

`release` 模块负责装配 Windows x64 免安装便携发行包，并提供基于 Ed25519 双重验签与事务回滚的软件内在线升级能力。构建与更新机制见 [ARCHITECTURE.md](ARCHITECTURE.md)，当前发行装配脚本见 [pack/README.md](../../pack/README.md)。

## 发行包组成要求

现场解压 ZIP 后直接启动，不在机主电脑安装任何脚本运行时、开发工具或内核驱动。

| 组成部分 | 发行规格 |
|---|---|
| 原生主程序 | 当前提供 `cst-updater.exe` 独立升级程序。阶段 9–11 统一切换为根目录提供 `cst-pilot.exe`（桌面与内置 TUI 主程序）、`cst-server.exe`（本机后台服务）与 `cst-updater.exe` |
| 随包诊断组件 | `wiztree/`（排除运行期 `WizTree3.ini`、`WizTree3.ini.bad` 与 `tmp/`，保留 `license.txt`）、`lhm/`（`LibreHardwareMonitorLib.dll` 0.9.6 及其依赖）与 `lhm/host/cst-lhm-host.exe`（自包含 .NET 10 隔离采集器及运行时许可） |
| 业务提示词与技能 | `agent/home/APPEND_SYSTEM.md`、`agent/home/skills/`（8 份维护技能说明），以及 `agent/home/extensions/telemetry/` 下的专用证书 `timserver_1.crt` 与币种映射 `currency.json` |
| 界面与字体资产 | `agent/home/extensions/web/static/` 预构建静态产物，以及双端共用的 `assets/fonts/SourceHanSansCN-VF.otf` 与 `assets/fonts/LICENSE.txt`。阶段 9 补充随包桌面等宽字体 `assets/fonts/JetBrainsMono-*.ttf` 及其许可证 |
| 预置标准配置 | 生成干净 `settings.json`、双端 `telemetry.json`、默认免费 Exa MCP 且仅放行 `198.18.0.0/15` 的 `web-search.json`，以及补齐默认模型 `deepseek/deepseek-flash` 的离线 `models-store.json` |
| 文档、许可与校验 | `README.md`、`CHANGELOG.md`、`LICENSE`、`THIRD-PARTY-NOTICES.md`、`licenses/`、`doc/`，以及根目录生成的 `VERSION`、`BUILD-INFO.json` 与全树 `SHA256SUMS` |

注：阶段 11 纯 Rust 打包工具切换前，仓库通过 `pack/pack.mjs` 维护 `v0.6.2` 发行包与干净树红线回归检查。

## Rust 软件内升级

软件内升级引擎（`crates/cst-kit/src/update/`、`crates/cst-updater/` 与 `cst-updater.exe`）满足以下安全与可靠性要求（双进程更新控制接口 `/api/app/update/*` 与双进程自检归属阶段 9 落地）：

1. **受控联网发现**：当前库层默认读取 `<oaHost>/api/agent/releases/latest` 获取签名版本清单（阶段 9 收敛为编译期显式注入 `CST_UPDATE_MANIFEST_URL`），启动后后台非阻塞检查一次并支持手动检查。仅在远端版本高于本机且本机不低于 `minUpgradableVersion` 时提示可升级，下载与安装须经本地确认。
2. **编译期信任根与双重验签**：发布构建通过 `CST_UPDATE_PUBLIC_KEY` 编译注入官方 Ed25519 公钥。缺少编译期升级参数时关闭升级入口，普通会话保持可用，不从远端清单动态读取信任根。升级包安装前必须通过清单签名、`SHA256SUMS` 独立签名、归档哈希及解压后全树逐文件 SHA-256 校验。
3. **程序区与用户数据区严格隔离**：升级过程原样保留 `agent/home/{auth,models,mcp-auth,web-settings,device}.json`（阶段 9 纳入 `desktop.json`）、`agent/home/{sessions,command-plans,telemetry,web-inbox,session-exports}/` 与 `artifacts/` 下的全部个人数据。四份预置配置（`settings.json`、`models-store.json`、`telemetry.json`、`web-search.json`）仅按缺失字段合并，保留用户已有设置。
4. **空闲门禁与遥测定稿**：启动交接前先关闭输入与写入门禁（`ActivityGate`），确认无运行中会话、排队输入或 BitLocker 操作。独立更新器启动后在共享 5 秒预算内完成遥测定稿，不阻塞等待网络上传。
5. **预写事务替换、进程自检与自动回滚**：独立更新器核验原宿主进程 PID 与创建时间并等待其退出后，按 `.state/update/transaction.json` 逐文件备份替换，随后在独立 Job Object 中执行新版 `--health-check`（15 秒总预算，阶段 9 覆盖双进程自检与启动自动 `--recover`）。自检失败、取消或替换中途断电重启时，按事务记录逆序恢复原版本文件。

## 干净发行树红线

无论是打包输出树还是在线升级归档，均执行以下六条强制校验，违反任一条立即拒绝发布或安装：

1. **禁止开发与测试文件**：不得包含任何路径下的 `AGENTS.md`、前端源码（`src/`）、测试目录或编译缓存。
2. **禁止运行态与个人数据**：不得包含 `.state/`、`.git/`、`sessions/`、`.cache/`、`artifacts/`、`outbox/`、`agent/home/fff/`、`agent/home/npm/`、`auth.json`、`models.json`、`mcp-auth.json`、`web-settings.json`、`device.json`、`identity.json`（阶段 9 纳入 `desktop.json`）、`telemetry/` 待发与草稿目录、`session-exports/`、`command-plans/`、`wiztree/tmp/`、`WizTree3.ini`、`WizTree3.ini.bad`、`.env` 或 `.env.*`。
3. **禁止越界路径与链接**：归档与发行树不得包含符号链接、Windows 目录联接（Junction）、硬链接、保留设备名、备用数据流（ADS）、`..` 越界路径或大小写冲突路径。Web 静态目录不得包含未裁剪的完整 `.otf` 源文件。
4. **禁止任何凭据或密钥**：全树扫描拒绝疑似 API 密钥、GitHub 令牌、云厂商密钥、JWT 或私钥块。所有 `.json` 递归检查禁止出现非空的 `apiKey`、`accessToken`、`refreshToken`、`clientSecret`、`identityKey`、`token`、`secret`、`password` 或 `authorization` 字段。
5. **预置配置精确匹配**：`agent/home/web-search.json` 与 `agent/home/telemetry.json` 必须与发行标准常量一致，禁止把构建机本机配置带入发行包。
6. **静态资源与许可证齐备**：入口 HTML 与 CSS 引用的所有本地静态资源、专用证书及第三方开源许可证文件必须全部存在于发行树内，且相对路径不得越出所属静态目录。

## 验收与发布条件

- 构建产物在隔离副本中通过冷／热启动、17 个开放工具白名单核验、后台对话回合与安全退出冒烟测试，测试运行态不进入正式 ZIP。
- 生成 ZIP 后解压至全新临时目录，逐文件复核 `SHA256SUMS` 与全部干净树红线。
- 实机 U 盘、跨机移动与在线升级回滚验收按 [../TESTING.md](../TESTING.md) 执行。
