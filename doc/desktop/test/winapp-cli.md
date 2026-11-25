# 桌面端 CLI 自动化操作手册（winapp）

本文说明如何使用 `winapp ui` 与 [desktop-secret.ps1](../../../scripts/dev/desktop-secret.ps1) 在本地驱动 `cst-pilot.exe` 完成节点检查、点击输入、无回显填密、单窗口截图与 MP4 录屏。架构设计与无障碍映射规则见 [9.1.desktop-automation.md](../../rust/plan/9.1.desktop-automation.md)，窗口规格见 [desktop/SPEC.md](../SPEC.md)。

---

## 环境准备

脚本要求 PowerShell 7；PrintWindow 录屏回退需要 PATH 中有支持 `libx264` 的 FFmpeg。通过 WinGet 安装 `winapp` 0.8.0 或更高版本，并关闭遥测：

```powershell
winget install Microsoft.winappcli --source winget
$env:WINAPP_CLI_TELEMETRY_OPTOUT = '1'
winapp --version
```

---

## 启动隔离桌面实例

调试时在 `E:/tmp/<YYYY-MM-DD>/` 或 `F:/tmp/<YYYY-MM-DD>/` 下构造独立工具包目录，避免污染本机真实配置，且支持与正在运行的开发版实例并存：

1. 使用 `cargo run -p xtask -- dist --out <runDir>` 组装干净发行树，或按阶段 9.2 目录契约复制字体、`app/prompts/APPEND_SYSTEM.md`、`app/skills/`、`app/defaults/`、`VERSION`、`cst-pilot.exe`、`app/cst-server.exe` 与 `app/cst-updater.exe`。
2. 设置 `CST_PILOT_ROOT`、`CST_OFFLINE=1` 与 `CST_ALLOW_UNELEVATED=1`（免弹 UAC 对话框），先用 `--port 0` 启动隔离后台服务，避免与其他实例争用端口。桌面端随后连接同一工具包的服务。
3. 按桌面进程 PID 调用 `winapp ui list-windows` 过滤主窗口句柄 `$hwnd`（排除系统 `100x100` 输入指示浮窗），首次执行一次 `inspect` 唤醒 GPUI 惰性无障碍树，后续命令统一携带 `-w $hwnd`。

```powershell
$dayDir = Join-Path 'E:/tmp' (Get-Date -Format 'yyyy-MM-dd')
$runDir = Join-Path $dayDir ('desktop-' + (Get-Date -Format 'HHmmssfff'))
$kitDir = "$runDir/kit"
$outDir = "$runDir/winapp"
New-Item -ItemType Directory -Force "$kitDir/app/fonts", "$kitDir/app/prompts", "$kitDir/app/skills", "$kitDir/app/defaults", "$kitDir/data/config", $outDir | Out-Null

Copy-Item -Path 'assets/fonts/*' -Destination "$kitDir/app/fonts/" -Recurse -Force
Copy-Item -Path 'app/prompts/APPEND_SYSTEM.md' -Destination "$kitDir/app/prompts/APPEND_SYSTEM.md" -Force
Copy-Item -Path 'app/skills/*' -Destination "$kitDir/app/skills/" -Recurse -Force
Copy-Item -Path 'app/defaults/*' -Destination "$kitDir/app/defaults/" -Recurse -Force
Set-Content -Path "$kitDir/VERSION" -Value 'cst-pilot v0.1.0' -Encoding utf8NoBOM
Copy-Item -Path 'target/release/cst-pilot.exe' -Destination "$kitDir/" -Force
Copy-Item -Path 'target/release/cst-server.exe', 'target/release/cst-updater.exe' -Destination "$kitDir/app/" -Force

$env:CST_PILOT_ROOT = $kitDir
$env:CST_OFFLINE = '1'
$env:CST_ALLOW_UNELEVATED = '1'
$env:WINAPP_CLI_TELEMETRY_OPTOUT = '1'

$kitArg = '"{0}"' -f $kitDir
$srv = Start-Process -FilePath "$kitDir/app/cst-server.exe" -ArgumentList @('--port', '0', '--kit-root', $kitArg) -WindowStyle Hidden -PassThru
Start-Sleep -Seconds 1
if ($srv.HasExited) { throw '隔离后台服务启动失败。' }
$proc = Start-Process -FilePath "$kitDir/cst-pilot.exe" -ArgumentList @('--kit-root', $kitArg) -PassThru
$hwnd = $null
for ($i = 0; $i -lt 15 -and -not $hwnd -and -not $proc.HasExited; $i++) {
    $wins = winapp ui list-windows -a $proc.Id --json | ConvertFrom-Json
    $hwnd = ($wins | Where-Object { $_.width -ge 400 -and $_.className -eq 'Zed::Window' } | Select-Object -First 1).hwnd
    if (-not $hwnd) { Start-Sleep -Milliseconds 500 }
}
if (-not $hwnd) { throw '未找到隔离桌面端主窗口。' }
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

登录页 APIKey（`login-apikey-input`）、设置页自定义服务密钥（`providers-apikey-input`）与 BitLocker 恢复密码框（`bitlocker-pwd-input-<call_id>`）默认显示掩码，且不向 UI Automation 返回明文。填写前请清空输入框并保持掩码开启。脚本运行时，目标窗口必须在前台。若从前台终端启动，可在命令前加 `Start-Sleep -Seconds 3;`，随后手动切到目标窗口。

| 传参方式 | 命令示例 |
|---|---|
| 从环境变量读取（推荐） | `pwsh -NoProfile -File scripts/dev/desktop-secret.ps1 -WindowHandle $hwnd -AutomationId login-apikey-input -FromEnv DEEPSEEK_API_KEY` |
| 从标准输入管道读取 | `$rawKey \| pwsh -NoProfile -File scripts/dev/desktop-secret.ps1 -WindowHandle $hwnd -AutomationId providers-apikey-input -FromStdin` |

脚本接受 1 至 4096 个不含空格的可打印 ASCII 字符。BitLocker 恢复密码使用 48 位数字，可用连字符分成八组。投递过程中请勿切换焦点。脚本因焦点变化中止时，请清空输入再试。

成功后仅输出脱敏摘要，例如 `{"ok":true,"automationId":"login-apikey-input","charCount":35,"redacted":true}`。

---

## 单窗口截图与并发录屏

默认以目标窗口为捕获对象。录制前保持密码输入框处于掩码状态；手动显示凭据后，明文可能出现在截图和视频中，也可被手动复制。

### 1. 整窗截图与局部控件裁剪

```powershell
# 截取整个桌面端窗口
winapp ui screenshot -w $hwnd -o "$outDir/full-window.png" --json

# 仅裁剪单个组件区域（如底部输入框卡片）
winapp ui screenshot composer-card-zwas1 -w $hwnd -o "$outDir/composer.png" --json
```

### 2. 边操作边录制 MP4 与毫秒帧序列

统一调用 [desktop-record.ps1](../../../scripts/dev/desktop-record.ps1) 启动后台录屏：交互桌面优先使用 `winapp ui record`。无法使用 winapp 且尚未生成产物时，尝试 `PrintWindow` 和 FFmpeg。请指定新的 `.mp4` 路径；脚本不会覆盖已有录屏或帧目录。启用 `-Frames` 时，`-MaxEdge` 取 64 至 4096。

```powershell
$rec = Start-Process -FilePath 'pwsh' -ArgumentList @(
  '-NoProfile', '-NonInteractive', '-File', 'scripts/dev/desktop-record.ps1',
  '-WindowHandle', "$hwnd",
  '-OutputPath', ('"{0}"' -f "$outDir/interaction.mp4"),
  '-DurationSec', '6', '-Fps', '15', '-MaxEdge', '1280', '-Frames'
) -NoNewWindow -PassThru

Start-Sleep -Milliseconds 300
winapp ui invoke sidebar-collapse-btn -w $hwnd --json; `
winapp ui wait-for topbar-expand-sidebar-btn -w $hwnd --timeout 2000 --json; `
winapp ui invoke topbar-expand-sidebar-btn -w $hwnd --json; `
winapp ui invoke sidebar-settings-btn -w $hwnd --json; `
winapp ui wait-for settings-tab-providers -w $hwnd --timeout 2000 --json

$rec.WaitForExit()
if ($rec.ExitCode -ne 0) { throw '录屏失败，请检查脚本报错。' }
```

`-Frames` 会生成同名 `.frames/` 目录，包含 JPEG 帧和 `frames.ndjson`。Winapp 还会生成 `manifest.json`；PrintWindow 回退从视频中提取变化帧，`elapsedMs` 表示帧在视频时间轴上的毫秒偏移。回退抓取的画面取决于窗口对 PrintWindow 的支持。

### 3. 清理隔离进程

通过桌面端的「退出 CST Pilot」关闭隔离实例。若之前手动启动的后台服务仍在运行，可用 `if (-not $srv.HasExited) { Stop-Process -InputObject $srv }` 关闭该进程。

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
