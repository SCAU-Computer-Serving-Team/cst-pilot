#Requires -Version 7.0
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter()]
    [string]$BinDir = (Join-Path $PSScriptRoot '..\..\target\release'),

    [Parameter()]
    [string]$OutputDir = '',

    [Parameter()]
    [ValidateSet('H-Dev', 'H-ThinLight', 'H-OldPC', 'H-WARP')]
    [string[]]$Profiles = @('H-Dev', 'H-ThinLight', 'H-OldPC', 'H-WARP'),

    [Parameter()]
    [ValidateRange(1, 10)]
    [int]$ColdStartRuns = 5
)

$ErrorActionPreference = 'Stop'
$env:WINAPP_CLI_TELEMETRY_OPTOUT = '1'

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
if ([string]::IsNullOrWhiteSpace($OutputDir)) {
    $dateStamp = Get-Date -Format 'yyyy-MM-dd'
    $baseTmp = if (Test-Path 'F:\') { 'F:\tmp' } else { 'E:\tmp' }
    $runId = [Guid]::NewGuid().ToString('N')
    $OutputDir = Join-Path $baseTmp "$dateStamp\desktop-ux-e2e-$runId"
}

$OutputDir = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputDir)
$BinDir = (Resolve-Path -LiteralPath $BinDir).ProviderPath
$fixtureModelCount = 400
. (Join-Path $PSScriptRoot 'desktop-job-throttle.ps1')

if (-not ('CstUxWin32' -as [type])) {
    Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;

public static class CstUxWin32
{
    public const uint WM_MOUSEWHEEL = 0x020A;

    [StructLayout(LayoutKind.Sequential)]
    public struct POINT { public int X; public int Y; }

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool MoveWindow(IntPtr hWnd, int X, int Y, int nWidth, int nHeight, bool bRepaint);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern IntPtr SetThreadDpiAwarenessContext(IntPtr value);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool PostMessageW(IntPtr hWnd, uint Msg, IntPtr wParam, IntPtr lParam);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool ClientToScreen(IntPtr hWnd, ref POINT lpPoint);

    private static IntPtr SetDpiContext(IntPtr value)
    {
        IntPtr previous = SetThreadDpiAwarenessContext(value);
        if (previous == IntPtr.Zero)
            throw new Win32Exception(Marshal.GetLastWin32Error(), "SetThreadDpiAwarenessContext failed");
        return previous;
    }

    public static void ResizeWindow(IntPtr hWnd, int x, int y, int width, int height)
    {
        IntPtr previous = SetDpiContext((IntPtr)(-4)); // PerMonitorV2
        try
        {
            if (!MoveWindow(hWnd, x, y, width, height, true))
                throw new Win32Exception(Marshal.GetLastWin32Error(), "MoveWindow failed");
        }
        finally { SetDpiContext(previous); }
    }

    public static void SendWheelAt(IntPtr hWnd, int delta, int clientX, int clientY)
    {
        IntPtr previous = SetDpiContext((IntPtr)(-4));
        POINT pt = new POINT { X = clientX, Y = clientY };
        try
        {
            if (!ClientToScreen(hWnd, ref pt))
                throw new Win32Exception(Marshal.GetLastWin32Error(), "ClientToScreen failed");
        }
        finally { SetDpiContext(previous); }
        SendWheelAtScreen(hWnd, delta, pt.X, pt.Y);
    }

    public static void SendWheelAtScreen(IntPtr hWnd, int delta, int x, int y)
    {
        if (x < short.MinValue || x > short.MaxValue || y < short.MinValue || y > short.MaxValue)
            throw new ArgumentException("Wheel screen point does not fit signed 16-bit coordinates.");
        IntPtr wParam = (IntPtr)((delta << 16) & unchecked((int)0xFFFF0000));
        IntPtr lParam = (IntPtr)(((y & 0xFFFF) << 16) | (x & 0xFFFF));
        if (!PostMessageW(hWnd, WM_MOUSEWHEEL, wParam, lParam))
            throw new Win32Exception(Marshal.GetLastWin32Error(), "PostMessageW failed");
    }
}
'@
}
function Get-Percentile {
    param([double[]]$Values, [double]$Percent)
    if (-not $Values -or $Values.Count -eq 0) { return 0.0 }
    $sorted = @($Values | Sort-Object)
    $idx = [int][Math]::Round(($Percent / 100.0) * ($sorted.Count - 1))
    $idx = [Math]::Max(0, [Math]::Min($sorted.Count - 1, $idx))
    return [Math]::Round([double]$sorted[$idx], 2)
}

function Invoke-ProbeRequest {
    param(
        [int]$Port,
        [string]$Method = 'GET',
        [string]$Path = '/probe'
    )
    return Invoke-RestMethod -Method $Method -Uri "http://127.0.0.1:$Port$Path" -NoProxy -TimeoutSec 5
}

function Invoke-UiaById {
    param(
        [intptr]$Hwnd,
        [string]$AutomationId,
        [int]$TimeoutMs = 3000
    )
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $root = [Windows.Automation.AutomationElement]::FromHandle($Hwnd)
    $cond = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::AutomationIdProperty,
        $AutomationId
    )
    while ($sw.ElapsedMilliseconds -lt $TimeoutMs) {
        $el = $root.FindFirst([Windows.Automation.TreeScope]::Descendants, $cond)
        if ($el) {
            $pattern = $null
            if ($el.TryGetCurrentPattern([Windows.Automation.InvokePattern]::Pattern, [ref]$pattern)) {
                ([Windows.Automation.InvokePattern]$pattern).Invoke()
                return $true
            }
            if ($el.TryGetCurrentPattern([Windows.Automation.TogglePattern]::Pattern, [ref]$pattern)) {
                ([Windows.Automation.TogglePattern]$pattern).Toggle()
                return $true
            }
            if ($el.TryGetCurrentPattern([Windows.Automation.SelectionItemPattern]::Pattern, [ref]$pattern)) {
                ([Windows.Automation.SelectionItemPattern]$pattern).Select()
                return $true
            }
            return $false
        }
        Start-Sleep -Milliseconds 30
    }
    return $false
}

function Get-UiaBoundingRect {
    param(
        [intptr]$Hwnd,
        [string]$AutomationId,
        [int]$TimeoutMs = 2000
    )
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $root = [Windows.Automation.AutomationElement]::FromHandle($Hwnd)
    $cond = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::AutomationIdProperty,
        $AutomationId
    )
    while ($sw.ElapsedMilliseconds -lt $TimeoutMs) {
        $el = $root.FindFirst([Windows.Automation.TreeScope]::Descendants, $cond)
        if ($el) {
            $r = $el.Current.BoundingRectangle
            if (-not $r.IsEmpty -and $r.Width -gt 0 -and $r.Height -gt 0) {
                return [pscustomobject]@{
                    Left = [Math]::Round($r.Left, 2)
                    Top = [Math]::Round($r.Top, 2)
                    Right = [Math]::Round($r.Right, 2)
                    Bottom = [Math]::Round($r.Bottom, 2)
                }
            }
        }
        Start-Sleep -Milliseconds 30
    }
    return $null
}

function Get-UiaModelRows {
    param([intptr]$Hwnd)
    $root = [Windows.Automation.AutomationElement]::FromHandle($Hwnd)
    foreach ($el in $root.FindAll([Windows.Automation.TreeScope]::Descendants, [Windows.Automation.Condition]::TrueCondition)) {
        $current = $el.Current
        if ($current.AutomationId -match '^pref-enabled-row-(\d+)$') {
            $index = [int]$Matches[1]
            $rect = $current.BoundingRectangle
            if (-not $current.IsOffscreen -and -not $rect.IsEmpty -and $rect.Width -gt 0 -and $rect.Height -gt 0) {
                [pscustomobject]@{ Index = $index; Rect = $rect }
            }
        }
    }
}

function Test-UiaElementPresent {
    param([intptr]$Hwnd, [string]$AutomationId)
    $root = [Windows.Automation.AutomationElement]::FromHandle($Hwnd)
    $cond = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::AutomationIdProperty, $AutomationId)
    return ($null -ne $root.FindFirst([Windows.Automation.TreeScope]::Descendants, $cond))
}

function Wait-DesktopWindowHandle {
    param([Diagnostics.Process]$Process, [int]$TimeoutMs = 10000)
    $sw = [Diagnostics.Stopwatch]::StartNew()
    while ($sw.ElapsedMilliseconds -lt $TimeoutMs) {
        if ($Process.HasExited) {
            throw "隔离桌面端提前退出（exit=$($Process.ExitCode)）。"
        }
        $raw = & winapp ui list-windows -a $Process.Id --json 2>$null
        if ($LASTEXITCODE -eq 0 -and $raw) {
            $wins = @($raw | ConvertFrom-Json)
            $win = $wins | Where-Object { $_.width -ge 400 -and $_.className -eq 'Zed::Window' } | Select-Object -First 1
            if ($win) {
                return [intptr][int64]$win.hwnd
            }
        }
        Start-Sleep -Milliseconds 40
    }
    throw "等待 PID=$($Process.Id) 主窗口超时（${TimeoutMs}ms）。"
}

function Expand-IsolatedKit {
    param([string]$KitRoot, [string]$SourceBinDir)

    if (Test-Path -LiteralPath $KitRoot) {
        throw "隔离目录已存在，请使用新的 -OutputDir：$KitRoot"
    }
    $homeDir = Join-Path $KitRoot 'agent\home'
    $sessionsDir = Join-Path $homeDir 'sessions'
    $skillsDir = Join-Path $homeDir 'skills'
    $fontsDir = Join-Path $KitRoot 'assets\fonts'
    New-Item -ItemType Directory -Force -Path $sessionsDir, $skillsDir, $fontsDir, (Join-Path $KitRoot '.state') | Out-Null

    foreach ($exe in @('cst-pilot.exe', 'cst-server.exe', 'cst-updater.exe')) {
        $src = Join-Path $SourceBinDir $exe
        if (-not (Test-Path -LiteralPath $src)) {
            throw "缺少 Release 可执行文件：$src"
        }
        Copy-Item -LiteralPath $src -Destination (Join-Path $KitRoot $exe) -Force
    }

    $verSrc = Join-Path $repoRoot 'VERSION'
    $verDst = Join-Path $KitRoot 'VERSION'
    if (Test-Path -LiteralPath $verSrc) {
        Copy-Item -LiteralPath $verSrc -Destination $verDst -Force
    } else {
        Set-Content -LiteralPath $verDst -Value 'cst-pilot 0.1.0' -Encoding utf8NoBOM
    }
    Copy-Item -Path (Join-Path $repoRoot 'assets\fonts\*') -Destination $fontsDir -Force
    Copy-Item -LiteralPath (Join-Path $repoRoot 'agent\home\APPEND_SYSTEM.md') -Destination (Join-Path $homeDir 'APPEND_SYSTEM.md') -Force
    Copy-Item -Path (Join-Path $repoRoot 'agent\home\skills\*') -Destination $skillsDir -Recurse -Force

    # 1. Generate F-AuthAndModels + F-IconWall + F-LargeModels (400 enabled models catalog)
    $modelItems = for ($i = 1; $i -le $fixtureModelCount; $i++) {
        @{
            id = "ux-bench-model-$('{0:D3}' -f $i)"
            name = "Bench Model $('{0:D3}' -f $i)"
            api = 'openai-completions'
            reasoning = ($i % 2 -eq 0)
            input = @('text', 'image')
            cost = @{ input = 0; output = 0; cacheRead = 0; cacheWrite = 0 }
            contextWindow = 128000
            maxTokens = 8192
        }
    }
    $modelsJson = @{
        providers = @{
            'ux-bench' = @{
                baseUrl = 'http://127.0.0.1:9/v1'
                apiKey = 'test-offline-key'
                api = 'openai-completions'
                models = $modelItems
            }
        }
    } | ConvertTo-Json -Depth 6
    Set-Content -LiteralPath (Join-Path $homeDir 'models.json') -Value $modelsJson -Encoding utf8NoBOM

    @{
        enabledModels = @($modelItems | ForEach-Object { "ux-bench/$($_.id)" })
    } | ConvertTo-Json -Depth 3 | Set-Content -LiteralPath (Join-Path $homeDir 'settings.json') -Encoding utf8NoBOM

    $desktopSettings = @{
        theme = 'light'
        defaultView = 'tui'
        sidebarCollapsed = $false
        window = @{ width = 1280; height = 800 }
    } | ConvertTo-Json -Depth 4
    Set-Content -LiteralPath (Join-Path $homeDir 'desktop.json') -Value $desktopSettings -Encoding utf8NoBOM

    # 2. Generate F-LongHistory + F-MarkdownStress + F-DiagUnbreakable + F-IconWall (130 turns + 45 tool calls)
    $longLines = [Collections.Generic.List[string]]::new()
    $baseTs = [DateTimeOffset]::Parse('2026-11-21T08:00:00Z')
    $longLines.Add((@{
        type = 'session'; version = 3; id = 'sess-ux-long'
        timestamp = $baseTs.ToString('o'); cwd = 'C:\Users\Repair\Work'
    } | ConvertTo-Json -Compress))
    $longLines.Add((@{
        type = 'session_info'; id = 'info-long'; parentId = $null
        timestamp = $baseTs.AddSeconds(1).ToString('o')
        name = 'F-LongHistory 130轮+45工具压力排查与超长无断词路径校验_C_Windows_WinSxS_amd64_microsoft-windows-servicingstack_31bf3856ad364e35'
    } | ConvertTo-Json -Compress))

    $wideHeader = '| ' + ((1..18 | ForEach-Object { "Col$_" }) -join ' | ') + ' |'
    $wideSep = '| ' + ((1..18 | ForEach-Object { '---' }) -join ' | ') + ' |'
    $wideRow = '| ' + ((1..18 | ForEach-Object { "val_${_}_9981" }) -join ' | ') + ' |'
    $unbreakablePath = 'C:\Windows\WinSxS\amd64_microsoft-windows-servicingstack_31bf3856ad364e35_10.0.26100.1882_none_9a3b7c218d4e5f60\tiworker_diagnostic_payload_without_spaces_0123456789abcdef0123456789abcdef0123456789abcdef.etl'
    $eventlogRaw = ('[EventID 41 Kernel-Power] System rebooted without cleanly shutting down first. ' * 52)

    $parentId = $null
    for ($t = 1; $t -le 130; $t++) {
        $uid = "u-$t"
        $tsU = $baseTs.AddSeconds($t * 10)
        $uText = if ($t -eq 130) {
            "请汇总磁盘、驱动与事件日志根因，并核对路径 $unbreakablePath"
        } else {
            "第 $t 轮排查记录：检查系统磁盘占用、关键驱动状态与启动项变更。"
        }
        $longLines.Add((@{
            type = 'message'; id = $uid; parentId = $parentId; timestamp = $tsU.ToString('o')
            message = @{
                role = 'user'
                content = @(@{ type = 'text'; text = $uText })
                timestamp = $tsU.ToUnixTimeMilliseconds()
            }
        } | ConvertTo-Json -Depth 8 -Compress))
        $parentId = $uid

        # Attach 45 tool calls across turns 86..130 (all inside or near tail window for heavy F-IconWall + P3-1/P3-3)
        if ($t -gt 85) {
            $tcId = "tc-ux-$t"
            $aidTool = "at-$t"
            $toolName = @('disk', 'sys', 'eventlog', 'driver', 'startup')[$t % 5]
            $tsAt = $tsU.AddSeconds(1)
            $longLines.Add((@{
                type = 'message'; id = $aidTool; parentId = $parentId; timestamp = $tsAt.ToString('o')
                message = @{
                    role = 'assistant'
                    content = @(
                        @{ type = 'toolCall'; id = $tcId; name = $toolName; arguments = @{ action = 'overview'; turn = $t } }
                    )
                    api = 'openai-completions'; provider = 'ux-bench'; model = 'ux-bench-model-001'
                    usage = @{ input = 120; output = 40; cacheRead = 0; cacheWrite = 0; totalTokens = 160; cost = @{ input = 0; output = 0; cacheRead = 0; cacheWrite = 0; total = 0 } }
                    stopReason = 'toolUse'; timestamp = $tsAt.ToUnixTimeMilliseconds(); durationMs = 240
                }
            } | ConvertTo-Json -Depth 10 -Compress))
            $parentId = $aidTool

            $trId = "tr-$t"
            $tsTr = $tsU.AddSeconds(2)
            $rawText = if ($t -eq 129) { $eventlogRaw } else { "工具 $toolName 第 $t 轮采样正常，延迟 12ms，未发现硬件报错。" }
            $longLines.Add((@{
                type = 'message'; id = $trId; parentId = $parentId; timestamp = $tsTr.ToString('o')
                message = @{
                    role = 'toolResult'; toolCallId = $tcId; toolName = $toolName; isError = $false
                    content = @(@{ type = 'text'; text = $rawText })
                    details = @{ summary = "轮次 $t 采样完成"; path = $unbreakablePath }
                    timestamp = $tsTr.ToUnixTimeMilliseconds()
                }
            } | ConvertTo-Json -Depth 8 -Compress))
            $parentId = $trId
        }

        $aid = "a-$t"
        $tsA = $tsU.AddSeconds(4)
        $aText = if ($t -eq 130) {
            "### 综合诊断结论`n`n已核验路径 ``$unbreakablePath``。`n`n$wideHeader`n$wideSep`n$wideRow`n$wideRow`n`n``````powershell`nGet-Volume | Select-Object DriveLetter, SizeRemaining, Size`n``````"
        } else {
            "第 $t 轮分析完成：系统卷读写正常，当前状态稳定，无异常中断事件。"
        }
        $longLines.Add((@{
            type = 'message'; id = $aid; parentId = $parentId; timestamp = $tsA.ToString('o')
            message = @{
                role = 'assistant'
                content = @(@{ type = 'text'; text = $aText })
                api = 'openai-completions'; provider = 'ux-bench'; model = 'ux-bench-model-001'
                usage = @{ input = 200; output = 120; cacheRead = 0; cacheWrite = 0; totalTokens = 320; cost = @{ input = 0; output = 0; cacheRead = 0; cacheWrite = 0; total = 0 } }
                stopReason = 'stop'; timestamp = $tsA.ToUnixTimeMilliseconds(); durationMs = 310
            }
        } | ConvertTo-Json -Depth 10 -Compress))
        $parentId = $aid
    }
    Set-Content -LiteralPath (Join-Path $sessionsDir 'sess-ux-long.jsonl') -Value $longLines -Encoding utf8NoBOM

    # 3. Generate F-SessionMix (24 additional sessions -> 25 total sessions in sidebar)
    for ($s = 24; $s -ge 1; $s--) {
        $sid = "sess-mix-$('{0:D2}' -f $s)"
        $ts = $baseTs.AddMinutes(-$s * 5)
        $title = if ($s -eq 1) {
            '超长不间断标题测试_NVIDIA_RTX4090_Driver_566_36_Win11_24H2_BSOD_0x00000116_TDR_FAILURE_CHECKLIST'
        } else {
            "现场排查会话 $('{0:D2}' -f $s) - 硬件与驱动核验"
        }
        $lines = @(
            (@{ type = 'session'; version = 3; id = $sid; timestamp = $ts.ToString('o'); cwd = 'C:\Users\Repair' } | ConvertTo-Json -Compress),
            (@{ type = 'session_info'; id = "info-$sid"; parentId = $null; timestamp = $ts.AddSeconds(1).ToString('o'); name = $title } | ConvertTo-Json -Compress),
            (@{
                type = 'message'; id = "u-$sid"; parentId = $null; timestamp = $ts.AddSeconds(2).ToString('o')
                message = @{ role = 'user'; content = @(@{ type = 'text'; text = "检查会话 $sid 的系统健康摘要。" }); timestamp = $ts.AddSeconds(2).ToUnixTimeMilliseconds() }
            } | ConvertTo-Json -Depth 6 -Compress),
            (@{
                type = 'message'; id = "a-$sid"; parentId = "u-$sid"; timestamp = $ts.AddSeconds(5).ToString('o')
                message = @{
                    role = 'assistant'
                    content = @(@{ type = 'text'; text = "会话 $sid 核验通过：CPU、内存、磁盘与核心驱动状态均在正常阈值范围内。" })
                    api = 'openai-completions'; provider = 'ux-bench'; model = 'ux-bench-model-001'
                    usage = @{ input = 90; output = 60; cacheRead = 0; cacheWrite = 0; totalTokens = 150; cost = @{ input = 0; output = 0; cacheRead = 0; cacheWrite = 0; total = 0 } }
                    stopReason = 'stop'; timestamp = $ts.AddSeconds(5).ToUnixTimeMilliseconds(); durationMs = 180
                }
            } | ConvertTo-Json -Depth 8 -Compress)
        )
        $mixPath = Join-Path $sessionsDir "$sid.jsonl"
        Set-Content -LiteralPath $mixPath -Value $lines -Encoding utf8NoBOM
        (Get-Item -LiteralPath $mixPath).LastWriteTimeUtc = $ts.UtcDateTime
    }
    # Ensure F-LongHistory sorts at the very top of the sidebar recent list
    (Get-Item -LiteralPath (Join-Path $sessionsDir 'sess-ux-long.jsonl')).LastWriteTimeUtc = $baseTs.AddHours(1).UtcDateTime
}

function Close-IsolatedProcess {
    param([Diagnostics.Process]$Process)
    try {
        if (-not $Process.HasExited) {
            $Process.Kill($true)
            if (-not $Process.WaitForExit(3000)) {
                Write-Warning "等待隔离进程 PID=$($Process.Id) 退出超时。"
            }
        }
    } catch {
        Write-Warning "无法停止隔离进程 PID=$($Process.Id)：$($_.Exception.Message)"
    } finally {
        $Process.Dispose()
    }
}

function Save-DesktopScreenshot {
    param([intptr]$Hwnd, [string]$Path)
    $result = & winapp ui screenshot -w "$($Hwnd.ToInt64())" -o $Path --json
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "截图失败：$Path ($result)"
    }
}

function Get-ChatContrastRoi {
    param([pscustomobject]$Snapshot)
    $bounds = $Snapshot.bounds.workspace
    $scale = [double]$Snapshot.shell.scale_factor
    if ($Snapshot.shell.route -ne 'session:chat' -or $bounds.Count -ne 4 -or
        $bounds[2] -le 80.0 -or $bounds[3] -le 200.0 -or $scale -le 0.0) {
        throw '缺少当前对话截图的探针坐标。'
    }
    return @(
        [int][Math]::Round(($bounds[0] + 40.0) * $scale),
        [int][Math]::Round(($bounds[1] + 40.0) * $scale),
        [int][Math]::Round(($bounds[2] - 80.0) * $scale),
        [int][Math]::Round(($bounds[3] - 200.0) * $scale)
    )
}

function Open-IsolatedServer {
    param([string]$KitRoot)
    $serverExe = Join-Path $KitRoot 'cst-server.exe'
    $homeDir = Join-Path $KitRoot 'agent\home'
    $stateFile = Join-Path $KitRoot '.state\server.json'
    if (Test-Path -LiteralPath $stateFile) { Remove-Item -LiteralPath $stateFile -Force }

    $psi = [Diagnostics.ProcessStartInfo]::new($serverExe, "--kit-root `"$KitRoot`" --port 0")
    $psi.WorkingDirectory = $KitRoot
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.Environment['PI_CODING_AGENT_DIR'] = $homeDir
    $psi.Environment['PI_OFFLINE'] = '1'
    $psi.Environment['CST_ALLOW_UNELEVATED'] = '1'
    $proc = [Diagnostics.Process]::Start($psi)

    $ready = $false
    try {
        $sw = [Diagnostics.Stopwatch]::StartNew()
        while ($sw.ElapsedMilliseconds -lt 15000) {
            if ($proc.HasExited) {
                throw "隔离 cst-server.exe 提前退出（exit=$($proc.ExitCode)）。"
            }
            if (Test-Path -LiteralPath $stateFile) {
                try {
                    $info = Get-Content -LiteralPath $stateFile -Raw | ConvertFrom-Json
                    if ($info.port -and $info.token) {
                        $null = Invoke-RestMethod -Uri "http://127.0.0.1:$($info.port)/api/health" -NoProxy -TimeoutSec 2
                        $result = [pscustomobject]@{
                            Process = $proc
                            Port = [int]$info.port
                            Token = [string]$info.token
                        }
                        $ready = $true
                        return $result
                    }
                } catch {
                    Write-Verbose $_.Exception.Message
                }
            }
            Start-Sleep -Milliseconds 100
        }
        throw '隔离 cst-server.exe 启动超时。'
    } finally {
        if (-not $ready) { Close-IsolatedProcess -Process $proc }
    }
}

function Open-DesktopInstance {
    param(
        [string]$KitRoot,
        [string]$ProbeFile,
        [bool]$ForceWarp,
        [string]$ThrottleProfile = 'H-Dev'
    )
    if (Test-Path -LiteralPath $ProbeFile) { Remove-Item -LiteralPath $ProbeFile -Force }
    $desktopExe = Join-Path $KitRoot 'cst-pilot.exe'
    $homeDir = Join-Path $KitRoot 'agent\home'

    $psi = [Diagnostics.ProcessStartInfo]::new($desktopExe, "--kit-root `"$KitRoot`"")
    $psi.WorkingDirectory = $KitRoot
    $psi.UseShellExecute = $false
    $psi.Environment['PI_CODING_AGENT_DIR'] = $homeDir
    $psi.Environment['PI_OFFLINE'] = '1'
    $psi.Environment['CST_ALLOW_UNELEVATED'] = '1'
    $psi.Environment['CST_DESKTOP_UX_PROBE'] = '1'
    $psi.Environment['CST_DESKTOP_UX_PROBE_FILE'] = $ProbeFile
    if ($ForceWarp) {
        $psi.Environment['CST_DESKTOP_FORCE_WARP'] = '1'
    } else {
        $psi.Environment['CST_DESKTOP_FORCE_WARP'] = '0'
    }

    $launchSw = [Diagnostics.Stopwatch]::StartNew()
    $proc = [Diagnostics.Process]::Start($psi)
    $ready = $false
    $throttle = $null
    try {
        if ($ThrottleProfile -eq 'H-OldPC') {
            $throttle = Invoke-DesktopJobThrottle -ProcessId $proc.Id -Cores 2 -CpuCapPercent 25 -MemoryLimitMB 512
        } elseif ($ThrottleProfile -eq 'H-ThinLight') {
            $throttle = Invoke-DesktopJobThrottle -ProcessId $proc.Id -Cores 4 -CpuCapPercent 40 -MemoryLimitMB 640 -BelowNormalPriority
        }

        $probePort = 0
        while ($launchSw.ElapsedMilliseconds -lt 15000) {
            if ($proc.HasExited) {
                throw "隔离桌面端提前退出（exit=$($proc.ExitCode)）。"
            }
            if (Test-Path -LiteralPath $ProbeFile) {
                try {
                    $pf = Get-Content -LiteralPath $ProbeFile -Raw | ConvertFrom-Json
                    if ($pf.pid -eq $proc.Id -and $pf.port -ge 1 -and $pf.port -le 65535) {
                        $probePort = [int]$pf.port
                        break
                    }
                } catch {
                    Write-Verbose $_.Exception.Message
                }
            }
            Start-Sleep -Milliseconds 30
        }
        if ($probePort -eq 0) { throw '等待桌面端 UX 探针就绪超时。' }

        $hwnd = Wait-DesktopWindowHandle -Process $proc -TimeoutMs 20000
        while ($launchSw.ElapsedMilliseconds -lt 25000) {
            if ($proc.HasExited) {
                throw "隔离桌面端提前退出（exit=$($proc.ExitCode)）。"
            }
            try {
                $snap = Invoke-ProbeRequest -Port $probePort -Path '/probe'
                if ($snap.pid -eq $proc.Id -and $null -ne $snap.startup.first_frame_ms) {
                    $result = [pscustomobject]@{
                        Process = $proc
                        Throttle = $throttle
                        ProbePort = $probePort
                        Hwnd = $hwnd
                        InteractiveMs = [Math]::Round($launchSw.Elapsed.TotalMilliseconds, 2)
                        InitialSnap = $snap
                    }
                    $ready = $true
                    return $result
                }
            } catch {
                Write-Verbose $_.Exception.Message
            }
            Start-Sleep -Milliseconds 30
        }
        throw '等待桌面端首帧超时。'
    } finally {
        if (-not $ready) {
            try { Close-IsolatedProcess -Process $proc }
            finally { if ($throttle) { $throttle.Dispose() } }
        }
    }
}

function Close-DesktopInstance {
    param(
        [pscustomobject]$Instance,
        [switch]$Graceful
    )
    if (-not $Instance) { return 0.0 }
    $sw = [Diagnostics.Stopwatch]::StartNew()
    try {
        if ($Graceful -and -not $Instance.Process.HasExited) {
            try {
                $clickedClose = Invoke-UiaById -Hwnd $Instance.Hwnd -AutomationId 'window-btn-close' -TimeoutMs 2000
                if (-not $clickedClose) { $null = $Instance.Process.CloseMainWindow() }
                $null = $Instance.Process.WaitForExit(5000)
            } catch { Write-Verbose $_.Exception.Message }
        }
    } finally {
        try { Close-IsolatedProcess -Process $Instance.Process }
        finally { if ($Instance.Throttle) { $Instance.Throttle.Dispose() } }
    }
    return [Math]::Round($sw.Elapsed.TotalMilliseconds, 2)
}

function Switch-ThemeMode {
    param([pscustomobject]$Server, [string]$Theme)
    $headers = @{ Authorization = "Bearer $($Server.Token)" }
    $body = @{ theme = $Theme } | ConvertTo-Json -Compress
    $null = Invoke-RestMethod -Method Patch -Uri "http://127.0.0.1:$($Server.Port)/api/app/settings" `
        -Headers $headers -ContentType 'application/json' -Body $body -NoProxy -TimeoutSec 5
}

function Restore-DesktopBaseline {
    param([pscustomobject]$Server)
    $headers = @{ Authorization = "Bearer $($Server.Token)" }
    $body = @{
        theme = 'light'
        sidebarCollapsed = $false
    } | ConvertTo-Json -Compress
    $null = Invoke-RestMethod -Method Patch -Uri "http://127.0.0.1:$($Server.Port)/api/app/settings" `
        -Headers $headers -ContentType 'application/json' -Body $body -NoProxy -TimeoutSec 5
}

# Reusing a kit can overwrite files or attach to an earlier run's server.
New-Item -ItemType Directory -Path $OutputDir | Out-Null
$kitRoot = Join-Path $OutputDir 'kit'
Expand-IsolatedKit -KitRoot $kitRoot -SourceBinDir $BinDir

$timelineRows = [Collections.Generic.List[object]]::new()
$visualChecks = [Collections.Generic.List[object]]::new()
$profileReports = [ordered]@{}
$assertionFailures = [Collections.Generic.List[string]]::new()

$server = $null
$inst = $null
$serverExitedGracefully = $false
$serverGracefulExitMs = $null
try {
    $server = Open-IsolatedServer -KitRoot $kitRoot
    $catalog = Invoke-RestMethod -Uri "http://127.0.0.1:$($server.Port)/api/models" `
        -Headers @{ Authorization = "Bearer $($server.Token)" } -NoProxy -TimeoutSec 5
    $fixtureIds = @(1..$fixtureModelCount | ForEach-Object { 'ux-bench/ux-bench-model-{0:D3}' -f $_ })
    $readyFixtureIds = @($catalog.models | Where-Object { $_.provider -eq 'ux-bench' -and $_.credentialReady } |
        ForEach-Object { "ux-bench/$($_.id)" })
    if ($readyFixtureIds.Count -ne $fixtureModelCount -or
        @($fixtureIds | Where-Object { $_ -notin $readyFixtureIds -or $_ -notin $catalog.enabled }).Count -ne 0) {
        throw "F-LargeModels: 隔离后端没有加载全部 $fixtureModelCount 个已启用模型。"
    }

    for ($profileIndex = 0; $profileIndex -lt $Profiles.Count; $profileIndex++) {
        $profileName = $Profiles[$profileIndex]
        $isOldPc = ($profileName -eq 'H-OldPC')
        $isThinLight = ($profileName -eq 'H-ThinLight')
        $isWarp = ($profileName -eq 'H-WARP')
        $shotDir = Join-Path $OutputDir "screenshots\$profileName"
        New-Item -ItemType Directory -Force -Path $shotDir | Out-Null
        $probeFile = Join-Path $kitRoot ".state\ux-probe-$profileName.json"
        Restore-DesktopBaseline -Server $server

        # --- P5-1: Cold start benchmark ---
        $runs = if ($isWarp) { 1 } else { $ColdStartRuns }
        $firstFrames = [Collections.Generic.List[double]]::new()
        $interactiveTimes = [Collections.Generic.List[double]]::new()
        for ($r = 1; $r -le $runs; $r++) {
            $inst = Open-DesktopInstance -KitRoot $kitRoot -ProbeFile $probeFile -ForceWarp:$isWarp -ThrottleProfile $profileName
            $firstFrames.Add([double]$inst.InitialSnap.startup.first_frame_ms)
            $interactiveTimes.Add([double]$inst.InteractiveMs)
            if ($r -lt $runs) {
                $null = Close-DesktopInstance -Instance $inst
                $inst = $null
                Start-Sleep -Milliseconds 250
            }
        }

        $jobLimits = if ($inst.Throttle) {
            @{
                logicalCores = $inst.Throttle.LogicalCores
                cpuRate = $inst.Throttle.CpuRate
                processCommitLimitBytes = $inst.Throttle.ProcessCommitLimitBytes
                belowNormalPriority = $inst.Throttle.BelowNormalPriority
            }
        }
        $port = $inst.ProbePort
        $hwnd = $inst.Hwnd
        Start-Sleep -Milliseconds 400
        $timelineRows.Add((Get-ProcessResourceSample -ProcessId $inst.Process.Id -Role 'desktop' -Phase "${profileName}:boot"))
        $timelineRows.Add((Get-ProcessResourceSample -ProcessId $server.Process.Id -Role 'server' -Phase "${profileName}:boot"))

        # Ensure standard 1280x800 logical window (convert to physical pixels using scale_factor)
        $winSf = [double]$inst.InitialSnap.shell.scale_factor
        if ($winSf -le 0.0) { $winSf = 1.0 }
        $preResizeFrames = [int64]$inst.InitialSnap.frame_pacing.total_frames
        [CstUxWin32]::ResizeWindow($hwnd, 40, 40, [int][Math]::Round(1280 * $winSf), [int][Math]::Round(800 * $winSf))
        $resizeWait = [Diagnostics.Stopwatch]::StartNew()
        while ($resizeWait.ElapsedMilliseconds -lt 1500) {
            $curSnap = Invoke-ProbeRequest -Port $port -Path '/probe'
            if ([Math]::Abs([double]$curSnap.shell.viewport_width - 1280.0) -le 32.0 -and
                [int64]$curSnap.frame_pacing.total_frames -gt $preResizeFrames) {
                break
            }
            Start-Sleep -Milliseconds 40
        }
        Start-Sleep -Milliseconds 250

        $homeShotLight = Join-Path $shotDir '01-home-light.png'
        Save-DesktopScreenshot -Hwnd $hwnd -Path $homeShotLight
        $homeSnap = Invoke-ProbeRequest -Port $port -Path '/probe'
        $homeSf = [double]$homeSnap.shell.scale_factor

        # --- P1-2 & P1-4: Icon buttons & frosted glass popovers ---
        # Each control calls in-process `ux_probe::mark_input()` inside its click handler so
        # cross-process UIA InvokePattern overhead never contaminates app input latency.
        $null = Invoke-ProbeRequest -Port $port -Method POST -Path '/probe/reset'
        $iconActions = @(
            'composer-context-ring-btn',
            'composer-context-ring-btn',
            'composer-model-picker-btn',
            'composer-model-picker-btn',
            'sidebar-collapse-btn',
            'topbar-expand-sidebar-btn',
            'sidebar-settings-btn',
            'sidebar-new-session-btn',
            'sidebar-all-sessions-btn',
            'sidebar-new-session-btn',
            'sidebar-collapse-btn',
            'topbar-recent-drawer-btn',
            'topbar-recent-drawer-btn',
            'topbar-expand-sidebar-btn',
            'composer-context-ring-btn'
        )
        $iconFailCount = 0
        foreach ($aid in $iconActions) {
            if (-not (Invoke-UiaById -Hwnd $hwnd -AutomationId $aid -TimeoutMs 2500)) {
                $iconFailCount++
            }
            Start-Sleep -Milliseconds 90
        }
        $p1Snap = Invoke-ProbeRequest -Port $port -Path '/probe'
        $popoverShot = Join-Path $shotDir '02-popover-light.png'
        Save-DesktopScreenshot -Hwnd $hwnd -Path $popoverShot
        $composerRect = Get-UiaBoundingRect -Hwnd $hwnd -AutomationId 'composer-card-zwas1'
        $ctxPopoverRect = Get-UiaBoundingRect -Hwnd $hwnd -AutomationId 'composer-context-popover-h5jg6d'
        $ctxPopoverAboveCard = ($null -ne $composerRect -and $null -ne $ctxPopoverRect -and $ctxPopoverRect.Bottom -le $composerRect.Top)
        # Close context popover and capture Model Picker upward floating geometry
        $null = Invoke-UiaById -Hwnd $hwnd -AutomationId 'composer-context-ring-btn' -TimeoutMs 2000
        Start-Sleep -Milliseconds 120
        $null = Invoke-UiaById -Hwnd $hwnd -AutomationId 'composer-model-picker-btn' -TimeoutMs 2000
        Start-Sleep -Milliseconds 260
        $modelPickerRect = Get-UiaBoundingRect -Hwnd $hwnd -AutomationId 'composer-model-picker-d8zzg'
        $modelPickerAboveCard = ($null -ne $composerRect -and $null -ne $modelPickerRect -and $modelPickerRect.Bottom -le $composerRect.Top)
        $modelPickerShot = Join-Path $shotDir '02b-model-picker-light.png'
        Save-DesktopScreenshot -Hwnd $hwnd -Path $modelPickerShot
        $null = Invoke-UiaById -Hwnd $hwnd -AutomationId 'composer-model-picker-btn' -TimeoutMs 2000
        Start-Sleep -Milliseconds 120
        if (-not $ctxPopoverAboveCard -or -not $modelPickerAboveCard) {
            $assertionFailures.Add("${profileName} P1-4: 两个浮层均应位于输入框上方。")
        }
        if ($iconFailCount -ne 0 -or $p1Snap.input_latency.count -ne $iconActions.Count) {
            $assertionFailures.Add("${profileName} P1-2: 控件调用失败 $iconFailCount 次，采样 $($p1Snap.input_latency.count)/$($iconActions.Count)。")
        }

        # --- P2-2: Switch settings tabs and check the legacy TUI option ---
        if (-not (Invoke-UiaById -Hwnd $hwnd -AutomationId 'sidebar-settings-btn' -TimeoutMs 2500)) {
            throw 'P2-2: 无法打开设置页。'
        }
        Start-Sleep -Milliseconds 120
        $null = Invoke-ProbeRequest -Port $port -Method POST -Path '/probe/reset'
        foreach ($tabId in @('settings-tab-providers', 'settings-tab-preferences', 'settings-tab-shortcuts', 'settings-tab-general')) {
            if (-not (Invoke-UiaById -Hwnd $hwnd -AutomationId $tabId -TimeoutMs 2500)) {
                throw "P2-2: 无法切换到 $tabId。"
            }
            Start-Sleep -Milliseconds 120
        }
        $p2TabsSnap = Invoke-ProbeRequest -Port $port -Path '/probe'
        if ($null -eq (Get-UiaBoundingRect -Hwnd $hwnd -AutomationId 'settings-general-tab')) {
            throw 'P2-2: 未进入常规设置，无法校验默认终端选项。'
        }
        $tuiViewOptionPresent = Test-UiaElementPresent -Hwnd $hwnd -AutomationId 'settings-view-tui'
        if ($tuiViewOptionPresent) {
            throw 'P2-2: 常规设置仍包含默认终端选项。'
        }

        # P3-5: UIA supplies physical screen coordinates for WM_MOUSEWHEEL.
        if (-not (Invoke-UiaById -Hwnd $hwnd -AutomationId 'settings-tab-preferences' -TimeoutMs 2500) -or
            $null -eq (Get-UiaBoundingRect -Hwnd $hwnd -AutomationId 'pref-enabled-row-0')) {
            throw 'P3-5: 未打开模型偏好页或找不到首个已启用模型。'
        }
        Start-Sleep -Milliseconds 250
        $prefStartRows = @(Get-UiaModelRows -Hwnd $hwnd | Sort-Object Index)
        if ($prefStartRows.Count -eq 0 -or $prefStartRows.Count -ge $fixtureModelCount) {
            throw 'P3-5: 模型列表的可见行数异常。'
        }
        $targetRow = $prefStartRows[[int][Math]::Floor($prefStartRows.Count / 2)].Rect
        $prefWheelX = [int][Math]::Round($targetRow.X + $targetRow.Width / 2)
        $prefWheelY = [int][Math]::Round($targetRow.Y + $targetRow.Height / 2)
        $null = Invoke-ProbeRequest -Port $port -Method POST -Path '/probe/reset'
        for ($w = 1; $w -le 40; $w++) {
            [CstUxWin32]::SendWheelAtScreen($hwnd, -240, $prefWheelX, $prefWheelY)
            Start-Sleep -Milliseconds 16
        }
        Start-Sleep -Milliseconds 150
        $prefDownRows = @(Get-UiaModelRows -Hwnd $hwnd | Sort-Object Index)
        for ($w = 1; $w -le 20; $w++) {
            [CstUxWin32]::SendWheelAtScreen($hwnd, 240, $prefWheelX, $prefWheelY)
            Start-Sleep -Milliseconds 16
        }
        Start-Sleep -Milliseconds 150
        $p3PrefScrollSnap = Invoke-ProbeRequest -Port $port -Path '/probe'
        $prefUpRows = @(Get-UiaModelRows -Hwnd $hwnd | Sort-Object Index)
        if ($prefDownRows.Count -eq 0 -or $prefUpRows.Count -eq 0 -or
            $prefDownRows[0].Index -le $prefStartRows[0].Index -or $prefUpRows[0].Index -ge $prefDownRows[0].Index -or
            $p3PrefScrollSnap.frame_pacing.window_frames -le 0) {
            throw 'P3-5: 滚轮未在两个方向更新模型行，或没有采到重绘帧。'
        }
        if (-not (Invoke-UiaById -Hwnd $hwnd -AutomationId 'sidebar-all-sessions-btn' -TimeoutMs 2500)) {
            throw 'P2-2: 无法打开全部会话。'
        }

        # --- P2-1: Switch 10 sidebar sessions ---
        $null = Invoke-ProbeRequest -Port $port -Method POST -Path '/probe/reset'
        $sessionIds = @('sess-ux-long') + (1..9 | ForEach-Object { "sess-mix-$('{0:D2}' -f $_)" })
        $sessSwitchFails = 0
        foreach ($sid in $sessionIds) {
            if (-not (Invoke-UiaById -Hwnd $hwnd -AutomationId "sidebar-session-row-$sid" -TimeoutMs 2500)) {
                $sessSwitchFails++
            }
            Start-Sleep -Milliseconds 120
        }
        $p2SessSnap = Invoke-ProbeRequest -Port $port -Path '/probe'
        $sessLatencies = @($p2SessSnap.input_latency.recent | ForEach-Object { [double]$_.latency_ms })
        if ($sessSwitchFails -ne 0 -or $sessLatencies.Count -ne $sessionIds.Count) {
            $assertionFailures.Add("${profileName} P2-1: 会话切换调用失败 $sessSwitchFails 次，采样 $($sessLatencies.Count)/$($sessionIds.Count)。")
        }

        # Switch back to F-LongHistory for P3, D1, D5, P5-2
        $framesBeforeLongChat = [int64]$p2SessSnap.frame_pacing.total_frames
        if (-not (Invoke-UiaById -Hwnd $hwnd -AutomationId 'sidebar-session-row-sess-ux-long' -TimeoutMs 3000)) {
            throw '未能打开 F-LongHistory。'
        }
        $chatReady = $false
        $chatWait = [Diagnostics.Stopwatch]::StartNew()
        while ($chatWait.ElapsedMilliseconds -lt 10000) {
            $d1WideSnap = Invoke-ProbeRequest -Port $port -Path '/probe'
            if ($d1WideSnap.shell.active_session_id -eq 'sess-ux-long' -and
                $d1WideSnap.scroll.max_offset_y -gt 36 -and
                [int64]$d1WideSnap.frame_pacing.total_frames -gt $framesBeforeLongChat) {
                $chatReady = $true
                break
            }
            Start-Sleep -Milliseconds 50
        }
        if (-not $chatReady) { throw '等待 F-LongHistory 绘制及滚动区域就绪超时。' }
        $chatLightShot = Join-Path $shotDir '03-long-chat-light.png'
        Save-DesktopScreenshot -Hwnd $hwnd -Path $chatLightShot

        # --- P3-1 & D5: 120 continuous wheel events on 130-turn F-LongHistory ---
        $wheelX = [int][Math]::Round(760 * $winSf)
        $wheelY = [int][Math]::Round(420 * $winSf)
        $null = Invoke-ProbeRequest -Port $port -Method POST -Path '/probe/reset'
        for ($w = 1; $w -le 80; $w++) {
            $null = [CstUxWin32]::SendWheelAt($hwnd, 180, $wheelX, $wheelY)
            Start-Sleep -Milliseconds 16
        }
        Start-Sleep -Milliseconds 150
        $afterScrollUpSnap = Invoke-ProbeRequest -Port $port -Path '/probe'
        for ($w = 1; $w -le 40; $w++) {
            $null = [CstUxWin32]::SendWheelAt($hwnd, -180, $wheelX, $wheelY)
            Start-Sleep -Milliseconds 16
        }
        Start-Sleep -Milliseconds 150
        $p3ScrollSnap = Invoke-ProbeRequest -Port $port -Path '/probe'

        # --- P3-3: Expand earlier turns & return to bottom ---
        # Scroll to top to reveal show-earlier-turns-btn
        for ($w = 1; $w -le 35; $w++) {
            $null = [CstUxWin32]::SendWheelAt($hwnd, 360, [int][Math]::Round(760 * $winSf), [int][Math]::Round(420 * $winSf))
            Start-Sleep -Milliseconds 12
        }
        Start-Sleep -Milliseconds 220
        $null = Invoke-ProbeRequest -Port $port -Method POST -Path '/probe/reset'
        $earlierClicked = Invoke-UiaById -Hwnd $hwnd -AutomationId 'show-earlier-turns-btn' -TimeoutMs 2500
        Start-Sleep -Milliseconds 200
        $p3ExpandSnap = Invoke-ProbeRequest -Port $port -Path '/probe'
        $returnBottomClicked = Invoke-UiaById -Hwnd $hwnd -AutomationId 'scroll-to-bottom-btn' -TimeoutMs 2500
        Start-Sleep -Milliseconds 180
        $afterBottomSnap = Invoke-ProbeRequest -Port $port -Path '/probe'

        # --- P2-3: 30-step resize 1600x900 -> 1024x640 (crosses 1120px auto-collapse) -> 1280x800 ---
        [CstUxWin32]::ResizeWindow($hwnd, 20, 20, [int][Math]::Round(1520 * $winSf), [int][Math]::Round(840 * $winSf))
        Start-Sleep -Milliseconds 250
        $null = Invoke-ProbeRequest -Port $port -Method POST -Path '/probe/reset'
        for ($step = 1; $step -le 30; $step++) {
            $rw = [int][Math]::Round((1520 - (496.0 * $step / 30.0)) * $winSf)
            $rh = [int][Math]::Round((840 - (200.0 * $step / 30.0)) * $winSf)
            [CstUxWin32]::ResizeWindow($hwnd, 20, 20, $rw, $rh)
            Start-Sleep -Milliseconds 16
        }
        Start-Sleep -Milliseconds 250
        $narrowSnap = Invoke-ProbeRequest -Port $port -Path '/probe'
        for ($step = 1; $step -le 20; $step++) {
            $rw = [int][Math]::Round((1024 + (256.0 * $step / 20.0)) * $winSf)
            $rh = [int][Math]::Round((640 + (160.0 * $step / 20.0)) * $winSf)
            [CstUxWin32]::ResizeWindow($hwnd, 40, 40, $rw, $rh)
            Start-Sleep -Milliseconds 16
        }
        Start-Sleep -Milliseconds 250
        $p2ResizeSnap = Invoke-ProbeRequest -Port $port -Path '/probe'

        # --- P5-2 & P5-3: 10s Static Chat Page Idle Zero-Redraw & Pixel Freeze Audit ---
        Start-Sleep -Milliseconds 400
        $null = Invoke-ProbeRequest -Port $port -Method POST -Path '/probe/reset'
        $idleSw = [Diagnostics.Stopwatch]::StartNew()
        $idleStartDesktop = Get-ProcessResourceSample -ProcessId $inst.Process.Id -Role 'desktop' -Phase "${profileName}:idle_start"
        $idleStartServer = Get-ProcessResourceSample -ProcessId $server.Process.Id -Role 'server' -Phase "${profileName}:idle_start"
        $timelineRows.Add($idleStartDesktop)
        $timelineRows.Add($idleStartServer)
        $idleShot0 = Join-Path $shotDir '04-idle-t0.png'
        Save-DesktopScreenshot -Hwnd $hwnd -Path $idleShot0

        for ($tick = 1; $tick -le 40; $tick++) {
            Start-Sleep -Milliseconds 250
            $timelineRows.Add((Get-ProcessResourceSample -ProcessId $inst.Process.Id -Role 'desktop' -Phase "${profileName}:idle"))
            if ($tick % 4 -eq 0) {
                $timelineRows.Add((Get-ProcessResourceSample -ProcessId $server.Process.Id -Role 'server' -Phase "${profileName}:idle"))
            }
        }
        $idleEndDesktop = Get-ProcessResourceSample -ProcessId $inst.Process.Id -Role 'desktop' -Phase "${profileName}:idle_end"
        $idleElapsedMs = $idleSw.Elapsed.TotalMilliseconds
        $idleShot10 = Join-Path $shotDir '04-idle-t10.png'
        Save-DesktopScreenshot -Hwnd $hwnd -Path $idleShot10
        $p5IdleSnap = Invoke-ProbeRequest -Port $port -Path '/probe'

        $logicalCores = [Environment]::ProcessorCount
        if ($null -eq $idleStartDesktop -or $null -eq $idleEndDesktop) {
            throw '缺少桌面端静置阶段的资源样本。'
        }
        $idleCpuDeltaMs = [Math]::Max(0.0, $idleEndDesktop.cpuMs - $idleStartDesktop.cpuMs)
        $idleCpuPct = [Math]::Round(($idleCpuDeltaMs / $idleElapsedMs / $logicalCores) * 100.0, 3)

        # --- P6-2: Repeated session & popover soak (memory / handle stability + latency drift) ---
        $soakBase = Get-ProcessResourceSample -ProcessId $inst.Process.Id -Role 'desktop' -Phase "${profileName}:soak_base"
        $firstRoundMs = [Collections.Generic.List[double]]::new()
        $lastRoundMs = [Collections.Generic.List[double]]::new()
        for ($iter = 1; $iter -le 20; $iter++) {
            $targetSid = if ($iter % 2 -eq 1) { 'sess-mix-01' } else { 'sess-ux-long' }
            $null = Invoke-ProbeRequest -Port $port -Method POST -Path '/probe/reset'
            $null = Invoke-UiaById -Hwnd $hwnd -AutomationId "sidebar-session-row-$targetSid" -TimeoutMs 2500
            Start-Sleep -Milliseconds 90
            $iterSnap = Invoke-ProbeRequest -Port $port -Path '/probe'
            if ($iterSnap.input_latency.recent.Count -gt 0) {
                $msVal = [double]$iterSnap.input_latency.recent[0].latency_ms
                if ($iter -le 5) { $firstRoundMs.Add($msVal) }
                if ($iter -ge 16) { $lastRoundMs.Add($msVal) }
            }
            $null = Invoke-UiaById -Hwnd $hwnd -AutomationId 'composer-context-ring-btn' -TimeoutMs 2000
            Start-Sleep -Milliseconds 50
            $null = Invoke-UiaById -Hwnd $hwnd -AutomationId 'composer-context-ring-btn' -TimeoutMs 2000
            Start-Sleep -Milliseconds 50
        }
        Start-Sleep -Seconds 3
        $soakAfter = Get-ProcessResourceSample -ProcessId $inst.Process.Id -Role 'desktop' -Phase "${profileName}:soak_after"
        $timelineRows.Add($soakAfter)

        # --- D7: Dark theme switch & screenshot for visual contrast check ---
        Switch-ThemeMode -Server $server -Theme 'dark'
        Start-Sleep -Milliseconds 450
        $chatDarkShot = Join-Path $shotDir '05-long-chat-dark.png'
        Save-DesktopScreenshot -Hwnd $hwnd -Path $chatDarkShot
        $chatDarkSnap = Invoke-ProbeRequest -Port $port -Path '/probe'
        Switch-ThemeMode -Server $server -Theme 'light'
        Start-Sleep -Milliseconds 250

        # D1 Geometry centering check (composer centered inside workspace <= 1px)
        $wsB = $d1WideSnap.bounds.workspace
        $compB = $d1WideSnap.bounds.composer
        $sf = [double]$d1WideSnap.shell.scale_factor
        if ($sf -le 0.0) { $sf = 1.0 }
        $centerErrPx = $null
        if ($wsB -and $compB) {
            $leftGap = [double]$compB[0] - [double]$wsB[0]
            $rightGap = ([double]$wsB[0] + [double]$wsB[2]) - ([double]$compB[0] + [double]$compB[2])
            $centerErrPx = [Math]::Round([Math]::Abs($leftGap - $rightGap), 2)
        }

        if ($compB.Count -ne 4 -or $compB[2] -le 0 -or $compB[3] -le 0) {
            throw '缺少浅色对话截图中的输入框坐标。'
        }
        $compBox = @(
            [int][Math]::Round([double]$compB[0] * $sf),
            [int][Math]::Round([double]$compB[1] * $sf),
            [int][Math]::Round([double]$compB[2] * $sf),
            [int][Math]::Round([double]$compB[3] * $sf)
        )

        # Queue visual pixel checks for this profile
        $visualChecks.Add(@{
            name = "${profileName}_idle_freeze_diff"
            kind = 'freeze_diff'
            imageA = $idleShot0
            imageB = $idleShot10
        })
        $visualChecks.Add(@{
            name = "${profileName}_chat_light_contrast"
            kind = 'contrast'
            image = $chatLightShot
            roi = Get-ChatContrastRoi -Snapshot $d1WideSnap
            minRatio = 4.5
        })
        $visualChecks.Add(@{
            name = "${profileName}_chat_dark_contrast"
            kind = 'contrast'
            image = $chatDarkShot
            roi = Get-ChatContrastRoi -Snapshot $chatDarkSnap
            minRatio = 4.5
        })
        $visualChecks.Add(@{
            name = "${profileName}_composer_elevation_light"
            kind = 'elevation'
            image = $chatLightShot
            box = $compBox
            isDark = $false
        })
        if ($isWarp) {
            $visualChecks.Add(@{
                name = "${profileName}_home_fallback_heading_contrast"
                kind = 'contrast'
                image = $homeShotLight
                roi = @(
                    [int][Math]::Round(520 * $homeSf),
                    [int][Math]::Round(310 * $homeSf),
                    [int][Math]::Round(420 * $homeSf),
                    [int][Math]::Round(110 * $homeSf)
                )
                minRatio = 4.5
            })
        }

        $isLastProfile = ($profileIndex -eq $Profiles.Count - 1)
        $exitMs = Close-DesktopInstance -Instance $inst -Graceful:$isLastProfile
        $inst = $null
        if ($isLastProfile -and $server -and $server.Process) {
            $srvGraceSw = [Diagnostics.Stopwatch]::StartNew()
            $serverExitedGracefully = $server.Process.WaitForExit(5000)
            $serverGracefulExitMs = [Math]::Round($srvGraceSw.Elapsed.TotalMilliseconds, 2)
        }

        $firstMed = Get-Percentile -Values $firstRoundMs.ToArray() -Percent 50
        $lastMed = Get-Percentile -Values $lastRoundMs.ToArray() -Percent 50

        $profileReports[$profileName] = [ordered]@{
            hardware = @{
                throttledJobObject = ($isOldPc -or $isThinLight)
                throttleProfile = $profileName
                job = $jobLimits
                forcedWarp = $isWarp
                supportsSm50 = $d1WideSnap.surface.supports_sm50
                isSoftwareAdapter = $d1WideSnap.surface.is_software_adapter
            }
            p5_1_coldStart = @{
                runs = $runs
                firstFrameMedianMs = Get-Percentile -Values $firstFrames.ToArray() -Percent 50
                interactiveMedianMs = Get-Percentile -Values $interactiveTimes.ToArray() -Percent 50
                allFirstFrameMs = $firstFrames
                allInteractiveMs = $interactiveTimes
            }
            p1_controls = @{
                iconFailCount = $iconFailCount
                contextPopoverAboveComposer = $ctxPopoverAboveCard
                modelPickerAboveComposer = $modelPickerAboveCard
                inputSampleCount = $p1Snap.input_latency.count
                inputP50Ms = [Math]::Round([double]$p1Snap.input_latency.p50_ms, 2)
                inputP95Ms = [Math]::Round([double]$p1Snap.input_latency.p95_ms, 2)
                inputMaxMs = [Math]::Round([double]$p1Snap.input_latency.max_ms, 2)
                paintP95Ms = [Math]::Round([double]$p1Snap.frame_pacing.paint_us_p95 / 1000.0, 2)
                intervalP50Ms = [Math]::Round([double]$p1Snap.frame_pacing.interval_us_p50 / 1000.0, 2)
                intervalP95Ms = [Math]::Round([double]$p1Snap.frame_pacing.interval_us_p95 / 1000.0, 2)
                frostedDraws = $p1Snap.surface.frosted_draw_count
            }
            p2_switching = @{
                sessionSwitchFails = $sessSwitchFails
                sessionSwitchP50Ms = Get-Percentile -Values $sessLatencies -Percent 50
                sessionSwitchP95Ms = Get-Percentile -Values $sessLatencies -Percent 95
                sessionPaintP95Ms = [Math]::Round([double]$p2SessSnap.frame_pacing.paint_us_p95 / 1000.0, 2)
                settingsTabsInputP95Ms = [Math]::Round([double]$p2TabsSnap.input_latency.p95_ms, 2)
                settingsTabsOver16ms = $p2TabsSnap.frame_pacing.frames_over_16ms
                tuiDefaultViewRemoved = (-not $tuiViewOptionPresent)
                prefFixtureModelCount = $readyFixtureIds.Count
                prefVirtualScrollFrames = $p3PrefScrollSnap.frame_pacing.window_frames
                prefVirtualScrollRenderP95Ms = [Math]::Round([double]$p3PrefScrollSnap.frame_pacing.render_us_p95 / 1000.0, 2)
                prefVirtualScrollPaintP95Ms = [Math]::Round([double]$p3PrefScrollSnap.frame_pacing.paint_us_p95 / 1000.0, 2)
                prefVirtualScrollOver16ms = $p3PrefScrollSnap.frame_pacing.frames_over_16ms
                resizePaintP95Ms = [Math]::Round([double]$p2ResizeSnap.frame_pacing.paint_us_p95 / 1000.0, 2)
                resizeIntervalP95Ms = [Math]::Round([double]$p2ResizeSnap.frame_pacing.interval_us_p95 / 1000.0, 2)
                resizeOver16ms = $p2ResizeSnap.frame_pacing.frames_over_16ms
                narrowAutoCollapsed = [bool]$narrowSnap.shell.sidebar_collapsed
            }
            p3_scrolling = @{
                framesSampled = $p3ScrollSnap.frame_pacing.window_frames
                renderP50Ms = [Math]::Round([double]$p3ScrollSnap.frame_pacing.render_us_p50 / 1000.0, 2)
                renderP95Ms = [Math]::Round([double]$p3ScrollSnap.frame_pacing.render_us_p95 / 1000.0, 2)
                paintP50Ms = [Math]::Round([double]$p3ScrollSnap.frame_pacing.paint_us_p50 / 1000.0, 2)
                paintP95Ms = [Math]::Round([double]$p3ScrollSnap.frame_pacing.paint_us_p95 / 1000.0, 2)
                paintP99Ms = [Math]::Round([double]$p3ScrollSnap.frame_pacing.paint_us_p99 / 1000.0, 2)
                intervalP50Ms = [Math]::Round([double]$p3ScrollSnap.frame_pacing.interval_us_p50 / 1000.0, 2)
                intervalP95Ms = [Math]::Round([double]$p3ScrollSnap.frame_pacing.interval_us_p95 / 1000.0, 2)
                over16msFrames = $p3ScrollSnap.frame_pacing.frames_over_16ms
                over33msFrames = $p3ScrollSnap.frame_pacing.frames_over_33ms
                scrollUpDetachedFollow = (-not [bool]$afterScrollUpSnap.scroll.auto_follow_bottom)
                scrollBottomBtnShown = [bool]$afterScrollUpSnap.scroll.show_scroll_bottom_btn
                earlierTurnsClicked = $earlierClicked
                earlierExpandLatencyMs = [Math]::Round([double]$p3ExpandSnap.input_latency.max_ms, 2)
                returnBottomClicked = $returnBottomClicked
                reattachedFollow = [bool]$afterBottomSnap.scroll.auto_follow_bottom
            }
            p5_idleFreeze = @{
                windowFrames10s = $p5IdleSnap.frame_pacing.window_frames
                skyDrawDelta = $p5IdleSnap.surface.sky_draw_delta
                frostedDrawDelta = $p5IdleSnap.surface.frosted_draw_delta
                runningLightDelta = $p5IdleSnap.surface.running_light_delta
                cpuAvgPct = $idleCpuPct
                idlePrivateMb = $idleEndDesktop.privateMb
            }
            p6_soak = @{
                basePrivateMb = $soakBase.privateMb
                afterPrivateMb = $soakAfter.privateMb
                deltaPrivateMb = [Math]::Round($soakAfter.privateMb - $soakBase.privateMb, 2)
                baseHandles = $soakBase.handles
                afterHandles = $soakAfter.handles
                deltaHandles = ($soakAfter.handles - $soakBase.handles)
                firstRoundMedianMs = $firstMed
                lastRoundMedianMs = $lastMed
            }
            p7_shutdown = @{
                desktopExitMs = $exitMs
            }
            d1_geometry = @{
                workspaceBounds = $wsB
                composerBounds = $compB
                composerCenterErrorPx = $centerErrPx
            }
        }
    }
} finally {
    try {
        if ($inst) { $null = Close-DesktopInstance -Instance $inst }
    } finally {
        $serverExitSw = [Diagnostics.Stopwatch]::StartNew()
        if ($server) { Close-IsolatedProcess -Process $server.Process }
        $serverExitMs = [Math]::Round($serverExitSw.Elapsed.TotalMilliseconds, 2)
    }
}

# Export 250ms dual-process timeline CSV
$csvPath = Join-Path $OutputDir 'perf-timeline.csv'
$timelineRows | Where-Object { $null -ne $_ } | Export-Csv -Path $csvPath -NoTypeInformation -Encoding utf8NoBOM

# Run pure-Python visual pixel analyzer
$manifestPath = Join-Path $OutputDir 'visual-manifest.json'
$visualOutPath = Join-Path $OutputDir 'visual-audit.json'
@{ checks = $visualChecks } | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $manifestPath -Encoding utf8NoBOM
$pyAuditOut = & {
    # Pixel failures return 1 and still produce a report.
    $PSNativeCommandUseErrorActionPreference = $false
    & python (Join-Path $PSScriptRoot 'desktop-visual-audit.py') --manifest $manifestPath --output $visualOutPath
}
$visualExitCode = $LASTEXITCODE
if ($visualExitCode -gt 1 -or -not (Test-Path -LiteralPath $visualOutPath -PathType Leaf)) {
    throw "desktop-visual-audit.py 执行失败：$pyAuditOut"
}
$visualReport = Get-Content -LiteralPath $visualOutPath -Raw | ConvertFrom-Json

if ($visualExitCode -ne 0 -or $visualReport.allPassed -ne $true) {
    $assertionFailures.Add("像素检查未通过：$visualOutPath")
}

$fullReport = [ordered]@{
    generatedAt = (Get-Date).ToString('o')
    machine = @{
        logicalCores = [Environment]::ProcessorCount
        os = [Environment]::OSVersion.VersionString
        binDir = $BinDir
    }
    serverExitMs = $serverExitMs
    serverExitedGracefully = $serverExitedGracefully
    serverGracefulExitMs = $serverGracefulExitMs
    profiles = $profileReports
    assertionFailures = $assertionFailures
    visualAudit = $visualReport
}

$reportPath = Join-Path $OutputDir 'ux-perf-report.json'
$fullReport | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $reportPath -Encoding utf8NoBOM
Write-Output ($fullReport | ConvertTo-Json -Depth 10)
if ($assertionFailures.Count -ne 0) {
    throw ($assertionFailures -join [Environment]::NewLine)
}
