# 提示词

## 初始化

1. 若从未阅读过项目文档，按以下顺序阅读：
   1. 基本情况：`doc\SPEC.md`、`README.md`、`doc\README.md`。
   2. 架构、待办与测试：`doc\ARCHITECTURE.md`、`doc\rust\CRATES.md`、`doc\TASKS.md`、`doc\test\TESTING.md`、`doc\test\desktop-ui-audit.md`。
   3. 按任务阅读对应模块目录（`pi-core`、`server`、`diagnostics`、`auth`、`telemetry`、`web`、`desktop`、`release`）及 `doc\rust\` 下的子包设计文档。
2. 了解 `earendil-works/pi`。本项目 Rust 核心对齐上游 Pi `1.5.0`，遵循 Pi 工程规范。
3. 遵循 Rust 社区与大型工作区工程实践。
4. 优先使用 `pi-context` 提供的工具标注 checkpoint。仅在明确切换任务且上下文超过 400K 时执行 compact。(如果有)
5. 执行前端与设计任务时，遵循三项设计核心文件：[storybook.html](design/storybook.html)（动效、令牌与交互行为参考最高规范）、`design\*.pen` 画布（静态设计、Shader 与整体设计最高规范，以 [cst-pilot-desktop.pen](design/cst-pilot-desktop.pen) 为主流基准）及 [DESIGN.md](DESIGN.md)（文本最高规范）。[storybook.html](design/storybook.html) 的静态组件与静态基础变量严格对应各 `.pen` 设计稿，`.pen` 内容优先级更高；`storybook.html` 不得出现 `.pen` 未定义的静态组件和静态基础变量；交互瞬态与动效归属交互侧，不属于 `.pen` 静态范围。
6. 执行 `Pen.dev` 等前端设计任务前，阅读 `emil` 系与 `gpui` 系 SKILLS，并通过 MCP 获取 `pen.dev` 操作方式。

## 文档规范

1. **必须**根据 `timdocize` 和 `lieflat-less-ai-tone` 的指导编写与修改。
2. 同步遵守下方对话规范。

## 对话规范

1. **直白描述**。不用比喻、拟人、夸张，例如不写「吃掉退路」「白花力气」。
2. **中文母语者思维**。一句话说一件事，从句拆成独立句。不写「我们」「大家」，不加程度副词。
3. **结构化优先**。
   - 对比、映射、清单用表格。
   - 步骤用有序列表，并列项用无序列表。
   - 每节先给结论，再给理由。
   - 单段不超过 3 句。
4. **保持极高信噪比**。旧实现、历史决策等后续不再使用的信息一律删除，仅保留对当下和未来有价值的内容。
5. 描述 `pen` 画布节点时，同时提供 Node ID 和组件所在区域，避免只写裸 ID。
6. **设计时可视化**。涉及界面设计方案时，直接在画布中新建或修改节点供查看。

## 工作流

提交到远端前触发：
1. 任何设计相关的内容，必须确保`storybook.html`的静态/shader侧内容严格参考各种pen的设计文件。
2. 任何新决策都应当写`ADR.md`

## 运行与调试

本地执行任何 Cargo 编译、检查与测试（含触发编译的启动脚本）前，**必须**先设置当前 PowerShell 进程的 CPU 亲和性，预留 2 个逻辑核心给系统（16 核对应 `0x3FFF`，上限 87.5%）。**禁止**未设置 CPU 亲和性直接运行 `cargo`。`cargo` 与 `rustc` 等子进程自动继承该硬性限制：

```powershell
$keep = [Math]::Min(62, [Math]::Max(1, [Environment]::ProcessorCount - 2))
(Get-Process -Id $PID).ProcessorAffinity = [IntPtr](([int64]1 -shl $keep) - 1)
# 可选：调低进程优先级
# (Get-Process -Id $PID).PriorityClass = 'BelowNormal'
cargo check --workspace --all-targets --locked
```

项目内直接启动 Rust 桌面端（`--release` 编译并启动优化版，`--build` 增量重编开发版）：

```powershell
.\pi-desktop-rust.cmd
.\pi-desktop-rust.cmd --release
```

构建前端并启动 Rust Web 独立服务：

```powershell
npm run build --prefix src/web/frontend
.\pi-web-rust.cmd
```

过渡期便携入口：

```powershell
.\pi.cmd
.\pi-web.cmd
```

PowerShell 隔离配置目录调试（`cst-server` 校验上两级目录下存在 `agent\home`）：

```powershell
New-Item -ItemType Directory -Force 'E:\tmp\<YYYY-MM-DD>\kit\agent\home' | Out-Null
$env:PI_CODING_AGENT_DIR = 'E:\tmp\<YYYY-MM-DD>\kit\agent\home'
$env:PI_OFFLINE = '1'
```

桌面端 CLI 自动化、无回显填密与截图录屏（完整用法与选择器见 [winapp-cli.md](doc/desktop/test/winapp-cli.md)）：

```powershell
$env:WINAPP_CLI_TELEMETRY_OPTOUT = '1'
# $proc 与 $outDir 来自手册中的隔离启动步骤。
$hwnd = (winapp ui list-windows -a $proc.Id --json | ConvertFrom-Json | Where-Object { $_.width -ge 400 -and $_.className -eq 'Zed::Window' } | Select-Object -First 1).hwnd
if (-not $hwnd) { throw '未找到隔离桌面端主窗口。' }
$null = winapp ui inspect -w $hwnd --interactive; Start-Sleep -Milliseconds 150; winapp ui inspect -w $hwnd --interactive
# 使用隔离实例。手动将窗口置于前台，并确认目标密码框为空、掩码已开启。
pwsh -NoProfile -File scripts/dev/desktop-secret.ps1 -WindowHandle $hwnd -AutomationId login-apikey-input -FromEnv CST_DESKTOP_TEST_KEY
winapp ui screenshot -w $hwnd -o "$outDir/shot.png" --json
pwsh -NoProfile -File scripts/dev/desktop-record.ps1 -WindowHandle $hwnd -OutputPath "$outDir/demo.mp4" -DurationSec 5 -Frames
```
