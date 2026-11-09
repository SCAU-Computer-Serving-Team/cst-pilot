# 便携发行规格

`release` 模块负责将官方 Pi 运行时、随包诊断程序、预打包第三方扩展与预构建 Web 前端装配为即插即用的 Windows x64 便携发行包。构建与字体管线见 [ARCHITECTURE.md](ARCHITECTURE.md)，命令行用法见 [pack/README.md](../../pack/README.md)。

## 发行包组成要求

现场只需解压 ZIP 并运行 `pi.cmd`，不在机主电脑安装 Node、Python、npm 或下载依赖。

| 组成部分 | 发行规格 |
|---|---|
| Pi 运行时 | 锁定官方 Windows x64 `0.85.1` 二进制及配套资源，逐文件核验 SHA-256；`pi.exe` 装配为 `agent/.runtime/prx.bin` |
| 启动与品牌 | 根目录包含 `pi.cmd`、`pi-web.cmd`、`launcher.ps1`、`assets/logo.png`、`README.md`、`CHANGELOG.md`、`LICENSE`、`THIRD-PARTY-NOTICES.md` |
| 随包诊断组件 | `pwsh/`（排除 Schemas、preview 与远程安装脚本）、`wiztree/`（排除运行期 ini 与 tmp）、`lhm/`（0.9.6 `.NET 10` 运行时 DLL） |
| 第三方扩展 | `pi-web-access`（0.18.0）与 `@ff-labs/pi-fff`（0.10.6）经 esbuild 预打包至 `agent/home/packages/`，保留 `pi-fff` Windows x64 原生依赖链；`pi-open-tui`（0.2.10）原样复制 |
| 项目扩展与技能 | `agent/home/extensions/`（排除所有 `test/` 目录）、`agent/home/skills/`、`agent/home/APPEND_SYSTEM.md` |
| Web 导出运行依赖 | `puppeteer-core`（25.16.0）及其锁定依赖树复制到 `agent/node_modules/`，保留依赖包随附许可证；另附 `licenses/puppeteer-LICENSE.txt`，不包含浏览器或额外 Pi SDK |
| Web 静态资源 | 每次打包从源码重新构建至 `agent/home/extensions/web/static/`，包含三档 `CSTUISans-*.woff2` 子集字体、`subset.json` 与字体许可 |
| 预置配置 | 生成标准 `settings.json`、双端 `telemetry.json`、默认使用免费 Exa MCP 且仅放行 `198.18.0.0/15` 的 `web-search.json`，以及补齐默认模型 `deepseek/deepseek-flash` 的离线 `models-store.json` |
| 元数据与校验 | 根目录生成 `VERSION`、`BUILD-INFO.json` 与覆盖全树文件的 `SHA256SUMS` |

## Rust 诊断组件

Rust 业务包使用 LHM 时，`lhm/host/` 必须包含通过 [`publish.ps1`](../../crates/cst-pilot/lhm-host/publish.ps1) 生成的完整自包含 .NET 10 目录及运行时许可；`lhm/` 保留 LHM 0.9.6 与原依赖 DLL。构建机安装 SDK，发行包运行时不依赖机主 .NET，也不附带构建缓存或模拟 DLL。

## Rust 软件内升级

Rust 升级包须满足以下要求：

- 默认从 CSTOA 的 `/api/agent/releases/latest` 获取签名版本清单，启动后后台检查一次，支持手动检查。仅在新版高于本机版本且满足最低升级基线时提示可升级，安装须经本地确认。
- 发布构建通过 `CST_UPDATE_PUBLIC_KEY` 编译官方 Ed25519 公钥。未配置公钥时默认升级入口不可用，不发更新请求；公钥不从下载清单获取。
- 清单签名、`SHA256SUMS` 独立签名、归档摘要及全树哈希必须全部通过。归档仅包含 `cst-pilot/` 顶层目录，禁止链接、保留设备名、备用数据流、路径越界、大小写冲突、运行数据与凭据。
- Rust 包包含 `cst-pilot.exe`、`cst-updater.exe`、版本与构建信息、8 份技能、系统提示词、遥测证书和第三方许可。版本须高于本机版本，且本机满足最低可升级基线。
- 用户凭据、会话、遥测队列与 `artifacts/` 保持原样。四份预置配置按缺失字段合并，已有值、数组与动态模型条目保留；损坏配置报错并保留原文件。
- 升级前拒绝仍在运行的会话、排队输入、工具与配置写入；交接期间关闭新输入和业务写入。更新器启动成功后共用 5 秒遥测定稿预算，不等待网络上传；启动失败保持原会话可用。宿主退出全部已核验的交接进程，库不直接退出进程。
- 启动检测到未完成事务时，保留现场并先由独立更新器恢复，恢复前不加载业务资源。缺少正式公钥时只关闭更新入口，普通会话保持可用。
- 运行态替换由独立更新器在原进程退出后执行，核对 PID 与创建时间。新版须在 15 秒内通过只读健康检查，检查期间保留当前事务，普通业务启动仍须先完成恢复；文件替换中断可按事务恢复，现场或备份发生变化时停止并保留文件。通过健康检查的版本保持有效，界面重启失败单独报告。

清单协议与准备流程见 [ARCHITECTURE.md](ARCHITECTURE.md#rust-升级包准备)。生产签名公钥和发布源的签名清单由发行环境配置。

## 干净发行树红线

发行检查（`pack/release-checks.mjs`）对输出树执行强制校验，违反任一条即终止打包：

1. **禁止开发与测试文件**：不得包含任何路径下的 `AGENTS.md`、前端源码（`src/`）、扩展测试目录（`extensions/*/test/`）、文档测试目录或构建缓存。
2. **禁止运行态与个人数据**：不得包含 `.state/`、`.git/`、`sessions/`、`.cache/`、`auth.json`、`models.json`、`web-settings.json`、`agent/home/npm/`、`agent/home/fff/`、`agent/home/telemetry/`、`wiztree/tmp/`、`WizTree3.ini` 或 `.env`。
3. **禁止完整字体、裸入口与链接**：Web 静态目录不得包含完整 `.otf` 字体文件；发行根不得存在未改名的 `pi.exe`；发行树不得包含符号链接或目录联接。
4. **禁止任何凭据或密钥**：全树文件扫描拒绝疑似 API 密钥、GitHub 令牌、AWS 密钥、JWT 或私钥块；所有 `.json` 文件递归检查禁止出现非空的 `apiKey`、`accessToken`、`refreshToken`、`token`、`secret`、`password`、`authorization` 字段。
5. **配置严格匹配**：`agent/home/web-search.json` 与 `agent/home/telemetry.json` 必须与 `pack/release-settings.mjs` 定义的常量完全一致，禁止复制构建机本地配置。
6. **静态引用与许可完整**：`REQUIRED_RELEASE_FILES` 中的核心代码、证书（`timserver_1.crt`）、三方许可证（`licenses/`、`pwsh/LICENSE.txt`、`wiztree/license.txt`、字体许可）及 `index.html` / CSS 引用的所有静态资源必须全部存在于发行树中。CSS 相对引用按样式表位置解析，禁止越出 Web 静态目录或引用构建机文件。

## 验收与发布条件

- 构建命令必须在已提权终端中执行，自动在独立 `_smoke` 副本中完成首次／再次 CLI 启动、10 个核心工具注册核验、Web 服务启动与真实 `read` 工具回合冒烟，测试运行态不进入正式发行目录。
- 生成 ZIP 后解压到新临时目录，逐文件复核 `SHA256SUMS` 与干净树规则。
- 实机 U 盘、跨机与目标浏览器验收要求见 [../TESTING.md](../TESTING.md)。
