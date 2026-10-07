# 构建发行版

本页说明装配命令、输入与校验。运行组成见[便携发行方案](../doc/design/release.md)，体积与运行时依据见[可行性分析](../doc/web/research/release-feasibility.md)。

在项目根目录运行：

```powershell
node\node.exe pack\pack.mjs --official <官方ZIP或目录> --out <新的输出目录> --zip
```

脚本核对官方文件和扩展版本，从当前源码构建Web并检查字体，再在副本中验证首次/再次启动、模型工具注册及Web真实read续答，按清单生成ZIP并解压校验。无需真实密钥；测试副本保留在输出目录的 `_smoke` 下，不进入 ZIP。

发行树校验分两类：`checkReleaseTree` 拒绝项目`AGENTS.md`、运行态、凭据与密钥；`checkReleaseContent` 要求 Web 入口页与样式表引用的资源、随包许可证文件齐备，缺一即拒绝打包。装配完成后 `SHA256SUMS` 覆盖全部发行文件，并在打包后复核。

TUI 默认主题由 Pi 设置管理。Web 默认浅色，运行后保存到独立 `web-settings.json`，不复制构建机的外观选择。项目`AGENTS.md`不参与发行，运行指令由`agent/home/APPEND_SYSTEM.md`提供，见[运行注意事项](../doc/Notice.md)。

发行配置默认使用 OpenCode Go 的 `deepseek-flash`（DeepSeek V4.1 Flash）。打包时补全离线模型目录，不复制本机 `auth.json`；运行时由队员在本机登录。

`--version prev0.5` 指定试用包版本，VERSION、ZIP和BUILD-INFO同步；默认版本保持正式构建配置。

`--esbuild <本地esbuild入口>` 可复用已安装的 0.25.10。未指定时使用固定版本的 npx。`--skip-smoke` 仅用于检查装配目录，不能同时生成 ZIP。

扩展依赖锁在 `extensions.package.json` 和 `extensions.package-lock.json`。新环境将它们分别复制到 `agent/home/npm/package.json`、`package-lock.json`，再运行 `npm ci --ignore-scripts --prefix agent/home/npm`。升级依赖时一起更新锁文件并重新验收。

发布输出目录中的 ZIP、VERSION 和 SHA256SUMS。不要再次压缩运行过的目录。自动冒烟覆盖启动、工具注册与Web API实际工具链路，实际TUI待机/浏览器与跨机诊断仍需按 `doc/test/README.md` 验收。
