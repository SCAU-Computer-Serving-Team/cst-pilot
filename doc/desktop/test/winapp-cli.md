# 桌面端 CLI 自动化操作手册（winapp）

本文说明如何使用 `winapp ui` 与 [desktop-secret.ps1](../../../scripts/dev/desktop-secret.ps1) 在本地驱动 `cst-pilot.exe` 完成节点检查、点击输入、无回显填密、单窗口截图与 MP4 录屏。架构设计与无障碍映射规则见 [9.1.desktop-automation.md](../../rust/plan/9.1.desktop-automation.md)，窗口规格见 [desktop/SPEC.md](../SPEC.md)。

---

## 环境准备

通过 WinGet 安装微软官方 `winapp` 工具（要求 `v0.8.0` 及以上），并在当前终端关闭工具遥测：

```powershell
winget install Microsoft.winappcli --source winget
$env:WINAPP_CLI_TELEMETRY_OPTOUT = '1'
winapp --version
```

---

## 启动隔离桌面实例

调试时在 `E:/tmp/<YYYY-MM-DD>/` 或 `F:/tmp/<YYYY-MM-DD>/` 下构造独立工具包目录，避免污染本机真实配置，且支持与正在运行的开发版实例并存：

1. 创建当日临时工具包目录与输出目录，拷入随包字体、系统提示词、内置技能、版本文件以及编译产物 `cst-pilot.exe` 和 `cst-server.exe`。
2. 设置 `PI_CODING_AGENT_DIR`、`PI_OFFLINE=1` 与 `CST_ALLOW_UNELEVATED=1`（免弹 UAC 对话框），先以 `--port 0` 启动隔离后台服务（避免占用默认端口 `52831`），再传入 `--kit-root` 启动独立桌面端。
3. 按桌面进程 PID 调用 `winapp ui list-windows` 过滤主窗口句柄 `$hwnd`（排除系统 `100x100` 输入指示浮窗），首次执行一次 `inspect` 唤醒 GPUI 惰性无障碍树，后续命令统一携带 `-w $hwnd`。

```powershell
$dayDir = 'F:/tmp/2026-11-20'
$kitDir = "$dayDir/kit"
$outDir = "$dayDir/winapp"
New-Item -ItemType Directory -Force "$kitDir/assets/fonts", "$kitDir/agent/home/skills", $outDir | Out-Null

Copy-Item -Path 'assets/fonts/*' -Destination "$kitDir/assets/fonts/" -Recurse -Force
Copy-Item -Path 'agent/home/APPEND_SYSTEM.md' -Destination "$kitDir/agent/home/APPEND_SYSTEM.md" -Force
Copy-Item -Path 'agent/home/skills/*' -Destination "$kitDir/agent/home/skills/" -Recurse -Force
Set-Content -Path "$kitDir/VERSION" -Value 'cst-pilot 0.1.0' -Encoding utf8NoBOM
Copy-Item -Path 'target/debug/cst-pilot.exe', 'target/debug/cst-server.exe' -Destination "$kitDir/" -Force

$env:PI_CODING_AGENT_DIR = "$kitDir/agent/home"
$env:PI_OFFLINE = '1'
$env:CST_ALLOW_UNELEVATED = '1'
$env:WINAPP_CLI_TELEMETRY_OPTOUT = '1'

$srv = Start-Process -FilePath "$kitDir/cst-server.exe" -ArgumentList @('--port', '0', '--kit-root', $kitDir) -WindowStyle Hidden -PassThru
Start-Sleep -Milliseconds 600
$proc = Start-Process -FilePath "$kitDir/cst-pilot.exe" -ArgumentList @('--kit-root', $kitDir) -PassThru
Start-Sleep -Seconds 2

$wins = @(winapp ui list-windows -a $proc.Id --json | ConvertFrom-Json)
$hwnd = ($wins | Where-Object { $_.width -ge 400 -and $_.className -ne '#32770' } | Select-Object -First 1).hwnd
$null = winapp ui inspect -w $hwnd --interactive
Start-Sleep -Milliseconds 150
```

---

## 常用操作命令

PowerShell 中串联多条命令统一使用 `;` 分隔。常规点击与文本填写优先走后台 UIA 通道，不抢占宿主机鼠标指针。

| 操作场景 | 命令模板 |
|---|---|
| 列出目标窗口句柄 | `winapp ui list-windows -a cst-pilot --json` |
| 扫描当前可见交互控件 | `winapp ui inspect -w $hwnd --interactive` |
| 搜索含指定文案或 ID 的控件 | `winapp ui search "设置" -w $hwnd --json` |
| 后台点击按钮、切页、开关 | `winapp ui invoke <AutomationId> -w $hwnd --json` |
| 后台填写普通输入框与多行框 | `winapp ui set-value <AutomationId> "检查磁盘空间" -w $hwnd --json` |
| 读取普通输入框当前文本 | `winapp ui get-value <AutomationId> -w $hwnd --json` |
| 读取控件无障碍属性与状态 | `winapp ui get-property <AutomationId> -w $hwnd --json` |
| 等待异步节点出现或状态就绪 | `winapp ui wait-for <AutomationId> -w $hwnd --timeout 3000 --json` |
| 投递后台单键（如关闭浮层） | `winapp ui send-keys "esc" -w $hwnd --via post-message --json` |
| 触发真实悬停态（移动指针） | `winapp ui hover <AutomationId> -w $hwnd --dwell-time 300 --json` |
| 触发全局组合键（需置前窗口） | `winapp ui send-keys "ctrl+k" -w $hwnd --via send-input --json` |

---

## 无回显凭据输入

登录页 APIKey（`login-apikey-input`）、设置页自定义服务密钥（`providers-apikey-input`）与 BitLocker 恢复密码框（`bitlocker-pwd-input-<call_id>`）均开启掩码隐私保护，不支持外部读回。填写凭据统一使用 [desktop-secret.ps1](../../../scripts/dev/desktop-secret.ps1)：

| 传参方式 | 命令示例 |
|---|---|
| 从环境变量读取（推荐） | `pwsh -NoProfile -File scripts/dev/desktop-secret.ps1 -WindowHandle $hwnd -AutomationId login-apikey-input -FromEnv DEEPSEEK_API_KEY` |
| 从标准输入管道读取 | `$rawKey \| pwsh -NoProfile -File scripts/dev/desktop-secret.ps1 -WindowHandle $hwnd -AutomationId providers-apikey-input -FromStdin` |

脚本成功后仅输出脱敏摘要，例如 `{"ok":true,"automationId":"login-apikey-input","charCount":35,"redacted":true}`。

---

## 单窗口截图与并发录屏

`winapp ui screenshot` 与 `winapp ui record` 默认采用 `Windows.Graphics.Capture` 抓取目标窗口合成表面。即使终端窗口遮挡桌面端，导出的画面仍保持完整无遮挡。

### 1. 整窗截图与局部控件裁剪

```powershell
# 截取整个桌面端窗口
winapp ui screenshot -w $hwnd -o "$outDir/full-window.png" --json

# 仅裁剪单个组件区域（如底部输入框卡片）
winapp ui screenshot composer-card-zwas1 -w $hwnd -o "$outDir/composer.png" --json
```

### 2. 边操作边录制 MP4 与毫秒帧序列

统一调用 [desktop-record.ps1](../../../scripts/dev/desktop-record.ps1) 启动后台录屏：在常规交互桌面自动调用 `winapp ui record`（`Windows.Graphics.Capture`），在 Agent 隔离桌面（如 `exebox-*` 非输入桌面）自动回退至 `PrintWindow` + `ffmpeg` 管道编码，输出标准 H.264 `.mp4` 与 `<文件名>.frames/frames.ndjson` 变化帧序列：

```powershell
$rec = Start-Process -FilePath 'pwsh' -ArgumentList @(
  '-NoProfile', '-NonInteractive', '-File', 'scripts/dev/desktop-record.ps1',
  '-WindowHandle', "$hwnd",
  '-OutputPath', "$outDir/interaction.mp4",
  '-DurationSec', '6', '-Fps', '15', '-MaxEdge', '1280', '-Frames'
) -NoNewWindow -PassThru

Start-Sleep -Milliseconds 300
winapp ui invoke sidebar-collapse-btn -w $hwnd --json; `
winapp ui wait-for topbar-expand-sidebar-btn -w $hwnd --timeout 2000 --json; `
winapp ui invoke topbar-expand-sidebar-btn -w $hwnd --json; `
winapp ui invoke sidebar-settings-btn -w $hwnd --json; `
winapp ui wait-for settings-tab-providers -w $hwnd --timeout 2000 --json

$rec.WaitForExit()
```

录制结束后同级生成的 `interaction.frames/` 目录与 `frames.ndjson` 记录每次画面变化的 PNG 帧及 `elapsedMs` 毫秒偏移，供核验过渡时长与布局对齐。

### 3. 清理隔离进程

验证完成后关闭隔离桌面进程与隔离后台服务（切勿误杀开发机其他目录实例）：

```powershell
Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
Stop-Process -Id $srv.Id -Force -ErrorAction SilentlyContinue
```

---

## 高频 `AutomationId` 速查表

| 界面区域 | 常用选择器（`AutomationId`） |
|---|---|
| 侧边栏与顶栏 | `sidebar-collapse-btn`、`sidebar-new-session-btn`、`sidebar-search-box`、`sidebar-dashboard-btn`、`sidebar-all-sessions-btn`、`sidebar-session-row-<id>`、`sidebar-account-btn`、`sidebar-settings-btn`、`topbar-expand-sidebar-btn`、`topbar-recent-drawer-btn`、`topbar-view-gui-btn`、`topbar-view-tui-btn`、`topbar-open-web-btn`、`window-btn-minimize`、`window-btn-maximize`、`window-btn-close` |
| 聊天工作台 | `workspace-home-view`、`workspace-session-view`、`session-header-more-btn`、`composer-card-zwas1`、`composer-input`、`composer-attach-btn`、`composer-context-ring-btn`、`composer-model-picker-btn`、`composer-send-btn-b0mrik` |
| 登录与账号 | `login-view`、`login-tab-cstoa`、`login-tab-apikey`、`login-tab-oauth`、`login-apikey-provider-<id>`、`login-apikey-input`、`login-baseurl-input`、`login-apikey-save-btn`、`login-window-back-btn`、`cstoa-qr-image-box`、`account-view`、`account-quota-refresh-btn`、`account-logout-btn` |
| 设置中心 | `settings-view`、`settings-tab-general`、`settings-tab-providers`、`settings-tab-preferences`、`settings-tab-shortcuts`、`providers-id-input`、`providers-name-input`、`providers-baseurl-input`、`providers-apikey-input`、`preferences-search-input`、`pref-default-model-trigger` |
| 会话、分支与导出 | `all-sessions-view`、`all-sessions-manage-btn`、`all-sessions-exit-batch-btn`、`all-sessions-select-all-btn`、`all-sessions-batch-delete-btn`、`all-sessions-row-<id>`、`branch-tree-view`、`branch-tree-search-input`、`tree-filter-default`、`branch-tree-back-btn`、`export-view`、`export-format-png-btn`、`export-format-html-btn`、`export-save-btn` |
| 工具卡与弹窗 | `normal-tool-card-<call_id>`、`sys-overview-card-<call_id>`、`web-search-card-<call_id>`、`runbook-card-<call_id>`、`bitlocker-card-<call_id>`、`bitlocker-pwd-input-<call_id>`、`bitlocker-submit-btn-<call_id>`、`command-palette-modal`、`missing-key-dialog`、`update-dialog-card`、`web-active-lock-overlay`、`exit-confirm-dialog` |
