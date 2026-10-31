# 构建发行版

本页说明装配命令、输入与校验。发行组成与红线见[便携发行规格](../doc/release/SPEC.md)，装配管线见[发行与构建架构](../doc/release/ARCHITECTURE.md)。

在已取得管理员权限的终端中，从项目根目录运行。自动冒烟需要保持输入输出连接，未提权时直接报错，不弹出 UAC：

```powershell
node\node.exe pack\pack.mjs --official <官方ZIP或目录> --out <新的输出目录> --zip
```

脚本核对官方文件和扩展版本，从当前源码构建 Web 并检查字体，再在副本中验证首次/再次启动、模型工具注册及 Web 真实 read 续答，按清单生成 ZIP 并解压校验。无需真实密钥；测试副本保留在输出目录的 `_smoke` 下，不进入 ZIP。

发行树校验分两类：`checkReleaseTree` 拒绝项目 `AGENTS.md`、运行态、凭据与密钥；`checkReleaseContent` 要求 Web 入口页与样式表引用的资源、随包许可证文件齐备，缺一即拒绝打包。装配完成后 `SHA256SUMS` 覆盖全部发行文件，并在打包后复核。

TUI 默认主题由 Pi 设置管理。Web 默认浅色，运行后保存到独立 `web-settings.json`，不复制构建机的外观选择。项目 `AGENTS.md` 不参与发行，运行指令由 `agent/home/APPEND_SYSTEM.md` 提供，见[运行时规格](../doc/runtime/SPEC.md)。

发行配置默认使用 DeepSeek 官方的 `deepseek-flash`（DeepSeek V4.1 Flash）。缺少密钥时保留模型身份并提示配置，其他服务不会自动替代。离线目录沿用 Pi 上游 `https://pi.dev/api/models`；打包时保留已有上游条目，缺少默认模型时补全。不复制本机 `auth.json`；运行时由队员在本机登录。

默认构建版本为 `v0.5`，VERSION、ZIP 和 BUILD-INFO 同步。`--version <版本>` 可显式指定其他版本。

发行树生成 `agent/home/web-search.json`，通过 `searchProvider: "exa"` 使用无需密钥的 Exa MCP，并配置 `ssrf.allowRanges: ["198.18.0.0/15"]` 支持 TUN 假 IP 代理。启动器清理宿主搜索密钥与服务端点。`trustEnvProxy` 保持 false，本机地址、其他私网及重定向检查保留；发行检查拒绝额外字段、密钥或扩大放行范围，不复制构建机同名配置。

`--esbuild <本地esbuild入口>` 可复用已安装的 0.25.10。未指定时使用固定版本的 npx。`--skip-smoke` 仅用于检查装配目录，不能同时生成 ZIP。

扩展依赖锁在 `extensions.package.json` 和 `extensions.package-lock.json`。新环境将它们分别复制到 `agent/home/npm/package.json`、`package-lock.json`，再运行 `npm ci --ignore-scripts --legacy-peer-deps --prefix agent/home/npm`。共享 Pi SDK 由官方运行时提供；安装采用锁文件生成时的 peer 依赖模式，防止 npm 另装最新 SDK。

Web 导出另从 `agent/package-lock.json` 按依赖关系复制 `puppeteer-core` 及运行依赖至 `agent/node_modules/`。保留各包许可证，补入 `licenses/puppeteer-LICENSE.txt`，不附带浏览器；目标机需安装受支持的 Edge / Chrome。

升级依赖时一起更新锁文件并重新验收。

发布输出目录中的 ZIP、VERSION 和 SHA256SUMS。不要再次压缩运行过的目录。自动冒烟覆盖启动、工具注册与 Web API 实际工具链路，实际 TUI 待机/浏览器与跨机诊断仍需按 [测试规范](../doc/TESTING.md) 验收。
