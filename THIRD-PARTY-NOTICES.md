# 第三方组件声明（Third-Party Notices）

CST Pilot 发行版包含以下第三方组件。各组件以其原有许可证分发；本文件仅为汇总说明，许可证原文以各组件随包文件或上游仓库为准。本项目自身代码以 [MIT](LICENSE) 分发。

## 运行时与核心

| 组件 | 版本 | 许可证 | 来源 | 用途 |
|---|---|---|---|---|
| pi (pi.exe) | 0.85.1 | MIT | https://github.com/earendil-works/pi | Agent 运行时（官方 Windows 独立发行，内含 Bun 1.3.14） |
| puppeteer-core | 25.16.0 | Apache-2.0 | https://github.com/puppeteer/puppeteer | 会话导出浏览器控制；依赖树随包分发，保留各包许可证，主许可证见 `licenses/puppeteer-LICENSE.txt`；不附带或下载浏览器 |
| PowerShell | 7.6.5 | MIT | https://github.com/PowerShell/PowerShell | 诊断工具执行环境（随包 `pwsh/LICENSE.txt`） |
| fd | 10.5.0 | MIT / Apache-2.0（双许可） | https://github.com/sharkdp/fd | 文件查找（bin\fd.exe） |
| ripgrep | 15.2.0 | MIT / Unlicense（双许可） | https://github.com/BurntSushi/ripgrep | 内容搜索（bin\rg.exe） |

## pi 扩展（预打包或随包分发）

| 组件 | 版本 | 许可证 | 来源 | 用途 |
|---|---|---|---|---|
| pi-web-access | 0.18.0 | MIT (c) 2025 Nico Bailon | npm `pi-web-access` | 网页检索/抓取扩展（预打包） |
| pi-open-tui | 0.2.10 | MIT | npm `pi-open-tui` | TUI 文件选择器扩展（随包分发，含其 LICENSE） |
| @ff-labs/pi-fff | 0.10.6 | MIT | npm `@ff-labs/pi-fff` | 文件快速检索扩展（预打包） |
| @ff-labs/fff-bin-win32-x64 | 同 fff | MIT | npm | fff 检索引擎二进制 |
| @ff-labs/fff-node / fff-bun | 同 fff | MIT | npm | fff 运行时适配 |
| ffi-rs | 1.3.7 | MIT | npm `ffi-rs` | FFI 绑定（fff 原生调用） |
| @yuuang/ffi-rs-win32-x64-msvc | — | MIT | npm | ffi-rs 的 Windows 原生库 |
| @mariozechner/clipboard | 0.3.9 | MIT | npm | 剪贴板访问（pi 随带运行资源） |
| photon_rs_bg.wasm (@silvia-odwyer/photon) | — | MIT | https://github.com/silvia-odwyer/photon | 图像缩放/EXIF 处理（向模型发送图片前） |
| @sinclair/typebox | — | MIT | https://github.com/sinclairzx81/typebox | 校验 schema（由 pi 内置别名提供，不随包分发） |
| esbuild | 0.25.10 | MIT | https://github.com/evanw/esbuild | 仅构建时使用（扩展预打包），不出现在发行包 |

## 专用工具

| 组件 | 版本 | 许可证 | 来源 | 用途与条款 |
|---|---|---|---|---|
| ntfs-ng | 0.1.2 | MIT（原包为 MIT / Apache-2.0 双许可，本项目按 MIT 分发） | https://crates.io/crates/ntfs-ng | 纯 Rust NTFS `$MFT` 只读解析引擎，许可证原文见随包 `app/licenses/ntfs-ng-LICENSE.txt` |
| ib-matcher / ib-pinyin-data | 0.4.5 / 0.1.0 | MIT | https://crates.io/crates/ib-matcher | Rust 侧栏搜索与快速打开的中文拼音（全拼／简拼／多音字）匹配引擎 |
| strsim | 0.11.1 | MIT | https://crates.io/crates/strsim | Rust 侧栏搜索与快速打开的受限编辑距离模糊打分 |
| pinyin-match | 1.2.10 | MIT | https://www.npmjs.com/package/pinyin-match | Web 前端侧栏搜索与快速打开的中文拼音匹配库 |
| LibreHardwareMonitorLib | — | MPL-2.0 | https://github.com/LibreHardwareMonitor/LibreHardwareMonitor | 硬件传感器读取（sensor/gpu 工具），许可证原文见随包 `licenses/MPL-2.0.txt` |

## 字体

四者均为 SIL Open Font License 1.1。字体文件不存于开发仓库（`design/fonts/` 与 `assets/fonts/` 已忽略），发行时随包分发原版可变字体与许可证。OFL-1.1 允许再分发，条件是保留版权声明与许可证原文。

| 字体 | 版本 | 许可证 | 来源 | 用途 |
|---|---|---|---|---|
| Source Han Sans CN（思源黑体可变字体） | 以随包 `SourceHanSansCN-VF.otf` 为准 | OFL-1.1 | https://github.com/adobe-fonts/source-han-sans | 界面正文与中文控件（桌面端与 Web 端共用 `250–900` 字重轴，许可证见 `assets/fonts/LICENSE.txt`） |
| Inter | 以随包文件为准 | OFL-1.1 | https://github.com/rsms/inter | 产品名、模型名、页脚 |
| JetBrains Mono | v2.304（随包 `JetBrainsMono-Regular.ttf` / `JetBrainsMono-Medium.ttf` 及 Web 字体） | OFL-1.1 | https://github.com/JetBrains/JetBrainsMono | 桌面端与聊天页等宽代码及行内工具名（许可证见 `assets/fonts/JetBrainsMono-OFL.txt`） |
| Noto Sans SC | 以随包文件为准 | OFL-1.1 | https://github.com/notofonts/noto-cjk | 仅设计画布说明文字，不进界面 |

字体用法与分发边界见 [DESIGN.md · 令牌、用色与字体约束](DESIGN.md#令牌用色与字体约束)。

## 设计资源

| 资源 | 版本 | 许可证 | 来源 | 用途与条款 |
|---|---|---|---|---|
| transitions.dev 动效片段与开发工具 | 条款更新于 2026-10-10 | 动效片段：站点使用条款；CLI：MIT | https://transitions.dev/terms.html | 已取得访问权限的动效允许修改并随个人或商业产品分发，禁止将动效库本身重新打包分发；开发工具不进入发行包。用法见 [storybook.html](design/storybook.html) 与 [DESIGN.md · 动效与跨端交互契约](DESIGN.md#动效与跨端交互契约) |

## 说明

1. 本项目自身代码（diagnostics 工具、启动器、打包脚本、文档）以 MIT 分发，原文见仓库与发行包根目录的 `LICENSE`。
2. 发行包内 `VERSION` 与 `SHA256SUMS` 用于完整性校验；`SHA256SUMS` 覆盖装配完成时的发行树。
3. 上游 npm 包未随附许可证文本时，本文件只登记许可证类型与来源链接。当前属此情况的有 `@ff-labs/pi-fff` 系列（含 fff-bin-win32-x64、fff-node、fff-bun）与 `@yuuang/ffi-rs-win32-x64-msvc`，均为 MIT。
