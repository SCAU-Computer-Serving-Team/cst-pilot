# 便携发行规格

`release` 模块负责将官方 Pi 运行时、随包诊断程序、预打包第三方扩展与预构建 Web 前端装配为即插即用的 Windows x64 便携发行包。构建与字体管线见 [ARCHITECTURE.md](ARCHITECTURE.md)，命令行用法见 [pack/README.md](../../pack/README.md)。

## 发行包组成要求

现场只需解压 ZIP 并运行 `pi.cmd`，不在机主电脑安装 Node、Python、npm 或下载依赖。

| 组成部分 | 发行规格 |
|---|---|
| Pi 运行时 | 锁定官方 Windows x64 `0.85.1` 二进制及配套资源，逐文件核验 SHA-256；`pi.exe` 装配为 `agent/.runtime/prx.bin` |
| 启动与品牌 | 根目录包含 `pi.cmd`、`launcher.ps1`、`assets/logo.png`、`README.md`、`CHANGELOG.md`、`LICENSE`、`THIRD-PARTY-NOTICES.md` |
| 随包诊断组件 | `pwsh/`（排除 Schemas、preview 与远程安装脚本）、`wiztree/`（排除运行期 ini 与 tmp）、`lhm/`（0.9.6 `.NET 10` 运行时 DLL） |
| 第三方扩展 | `pi-web-access`（0.18.0）与 `@ff-labs/pi-fff`（0.10.6）经 esbuild 预打包至 `agent/home/packages/`，保留 `pi-fff` Windows x64 原生依赖链；`pi-open-tui`（0.2.10）原样复制 |
| 项目扩展与技能 | `agent/home/extensions/`（排除所有 `test/` 目录）、`agent/home/skills/`、`agent/home/APPEND_SYSTEM.md` |
| Web 静态资源 | 每次打包从源码重新构建至 `agent/home/extensions/web/static/`，包含三档 `CSTUISans-*.woff2` 子集字体、`subset.json` 与字体许可 |
| 预置配置 | 生成标准 `settings.json`、双端 `telemetry.json`、默认使用免费 Exa MCP 且仅放行 `198.18.0.0/15` 的 `web-search.json`，以及补齐默认模型 `deepseek/deepseek-flash` 的离线 `models-store.json` |
| 元数据与校验 | 根目录生成 `VERSION`、`BUILD-INFO.json` 与覆盖全树文件的 `SHA256SUMS` |

## 干净发行树红线

发行检查（`pack/release-checks.mjs`）对输出树执行强制校验，违反任一条即终止打包：

1. **禁止开发与测试文件**：不得包含任何路径下的 `AGENTS.md`、前端源码（`src/`）、扩展测试目录（`extensions/*/test/`）、文档测试目录或构建缓存。
2. **禁止运行态与个人数据**：不得包含 `.state/`、`.git/`、`sessions/`、`.cache/`、`auth.json`、`models.json`、`web-settings.json`、`agent/home/npm/`、`agent/home/fff/`、`agent/home/telemetry/`、`wiztree/tmp/`、`WizTree3.ini` 或 `.env`。
3. **禁止完整字体、裸入口与链接**：Web 静态目录不得包含完整 `.otf` 字体文件；发行根不得存在未改名的 `pi.exe`；发行树不得包含符号链接或目录联接。
4. **禁止任何凭据或密钥**：全树文件扫描拒绝疑似 API 密钥、GitHub 令牌、AWS 密钥、JWT 或私钥块；所有 `.json` 文件递归检查禁止出现非空的 `apiKey`、`accessToken`、`refreshToken`、`token`、`secret`、`password`、`authorization` 字段。
5. **配置严格匹配**：`agent/home/web-search.json` 与 `agent/home/telemetry.json` 必须与 `pack/release-settings.mjs` 定义的常量完全一致，禁止复制构建机本地配置。
6. **静态引用与许可完整**：`REQUIRED_RELEASE_FILES` 中的核心代码、证书（`timserver_1.crt`）、三方许可证（`licenses/`、`pwsh/LICENSE.txt`、`wiztree/license.txt`、字体许可）及 `index.html` / CSS 引用的所有静态资源必须全部存在于发行树中。

## 验收与发布条件

- 构建命令必须在已提权终端中执行，自动在独立 `_smoke` 副本中完成首次／再次 CLI 启动、10 个核心工具注册核验、Web 服务启动与真实 `read` 工具回合冒烟，测试运行态不进入正式发行目录。
- 生成 ZIP 后解压到新临时目录，逐文件复核 `SHA256SUMS` 与干净树规则。
- 实机 U 盘、跨机与目标浏览器验收要求见 [../TESTING.md](../TESTING.md)。
