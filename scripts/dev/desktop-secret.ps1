[CmdletBinding(DefaultParameterSetName = 'FromEnv', PositionalBinding = $false)]
param(
    [Parameter()]
    [ValidateNotNullOrEmpty()]
    [string]$TargetApp = 'cst-pilot',

    [Parameter()]
    [int64]$WindowHandle = 0,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$AutomationId,

    [Parameter(Mandatory = $true, ParameterSetName = 'FromEnv')]
    [ValidateNotNullOrEmpty()]
    [string]$FromEnv,

    [Parameter(Mandatory = $true, ParameterSetName = 'FromStdin')]
    [switch]$FromStdin
)

$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes

if (-not ('CstDesktopSecretWin32' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class CstDesktopSecretWin32
{
    public const uint WM_KEYDOWN = 0x0100;
    public const uint WM_KEYUP = 0x0101;
    public const uint WM_CHAR = 0x0102;
    public const uint VK_BACK = 0x08;
    public const uint VK_END = 0x23;
    public const uint SMTO_ABORTIFHUNG = 0x0002;

    [DllImport("user32.dll", ExactSpelling = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool IsWindow(IntPtr hWnd);

    [DllImport("user32.dll", ExactSpelling = true)]
    public static extern IntPtr GetForegroundWindow();

    [DllImport("user32.dll", ExactSpelling = true, SetLastError = true)]
    public static extern IntPtr SendMessageTimeoutW(
        IntPtr hWnd,
        uint Msg,
        UIntPtr wParam,
        IntPtr lParam,
        uint fuFlags,
        uint uTimeout,
        out UIntPtr lpdwResult);

    public static bool SendVirtualKeyTap(IntPtr hWnd, uint virtualKey)
    {
        UIntPtr res;
        IntPtr downLParam = new IntPtr(1);
        if (SendMessageTimeoutW(hWnd, WM_KEYDOWN, new UIntPtr(virtualKey), downLParam, SMTO_ABORTIFHUNG, 2000, out res) == IntPtr.Zero)
        {
            return false;
        }
        IntPtr upLParam = new IntPtr(unchecked((int)0xC0000001));
        SendMessageTimeoutW(hWnd, WM_KEYUP, new UIntPtr(virtualKey), upLParam, SMTO_ABORTIFHUNG, 2000, out res);
        return true;
    }

    public static bool SendCharUnit(IntPtr hWnd, ushort utf16Unit)
    {
        UIntPtr res;
        return SendMessageTimeoutW(
            hWnd,
            WM_CHAR,
            new UIntPtr(utf16Unit),
            new IntPtr(1),
            SMTO_ABORTIFHUNG,
            2000,
            out res) != IntPtr.Zero;
    }
}
'@
}

function Resolve-TargetWindow {
    param(
        [Parameter(Mandatory = $true)]
        [string]$AppSelector,

        [Parameter(Mandatory = $true)]
        [int64]$ExplicitHwnd
    )

    if ($ExplicitHwnd -gt 0) {
        $ptr = [intptr]$ExplicitHwnd
        if (-not [CstDesktopSecretWin32]::IsWindow($ptr)) {
            throw "指定的窗口句柄无效或已关闭：$ExplicitHwnd"
        }
        $element = [Windows.Automation.AutomationElement]::FromHandle($ptr)
        return @{
            Hwnd = $ptr
            Element = $element
        }
    }

    $parsedPid = 0
    if ([int]::TryParse($AppSelector, [ref]$parsedPid)) {
        $procs = @(Get-Process -Id $parsedPid -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne [intptr]::Zero })
    } else {
        $cleanName = $AppSelector -replace '\.exe$', ''
        $procs = @(Get-Process -Name $cleanName -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne [intptr]::Zero })
    }

    if ($procs.Count -eq 0) {
        throw "未找到具有主窗口的目标进程：$AppSelector"
    }
    if ($procs.Count -gt 1) {
        $pids = ($procs | ForEach-Object { "$($_.Id)(hwnd=$($_.MainWindowHandle.ToInt64()))" }) -join ', '
        throw "匹配到多个目标窗口（$pids），请通过 -WindowHandle 或进程 PID 精确指定。"
    }

    $hwndPtr = $procs[0].MainWindowHandle
    return @{
        Hwnd = $hwndPtr
        Element = [Windows.Automation.AutomationElement]::FromHandle($hwndPtr)
    }
}

$secret = $null
$chars = $null
$charCount = 0

try {
    if ($PSCmdlet.ParameterSetName -eq 'FromEnv') {
        $secret = [Environment]::GetEnvironmentVariable($FromEnv)
        if ([string]::IsNullOrEmpty($secret)) {
            throw "环境变量未设置或为空：$FromEnv"
        }
    } else {
        $line = [Console]::In.ReadLine()
        if ($null -ne $line) {
            $secret = $line.TrimEnd("`r", "`n")
        }
        if ([string]::IsNullOrEmpty($secret)) {
            throw '标准输入未提供有效凭据内容。'
        }
    }

    $chars = $secret.ToCharArray()
    $charCount = $chars.Length

    $target = Resolve-TargetWindow -AppSelector $TargetApp -ExplicitHwnd $WindowHandle
    $hwnd = [intptr]$target.Hwnd
    $winElement = [Windows.Automation.AutomationElement]$target.Element

    $condition = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::AutomationIdProperty,
        $AutomationId
    )
    $found = $winElement.FindAll([Windows.Automation.TreeScope]::Descendants, $condition)
    if ($found.Count -eq 0) {
        throw "未在目标窗口中找到 AutomationId 为 '$AutomationId' 的控件。"
    }
    if ($found.Count -gt 1) {
        throw "在目标窗口中匹配到 $($found.Count) 个同名控件 '$AutomationId'，已中止输入。"
    }

    $editEl = $found[0]
    if (-not $editEl.Current.IsEnabled) {
        throw "目标输入控件处于禁用状态：$AutomationId"
    }
    if ($editEl.Current.ControlType -ne [Windows.Automation.ControlType]::Edit) {
        throw "目标控件类型非法（实际为 $($editEl.Current.ControlType.ProgrammaticName)），仅允许输入控件。"
    }

    $patternObj = $null
    $usedValuePattern = $false
    if ($editEl.TryGetCurrentPattern([Windows.Automation.ValuePattern]::Pattern, [ref]$patternObj)) {
        $valPattern = [Windows.Automation.ValuePattern]$patternObj
        if (-not $valPattern.Current.IsReadOnly) {
            $valPattern.SetValue($secret)
            $usedValuePattern = $true
        }
    }

    if (-not $usedValuePattern) {
        if ($editEl.Current.IsOffscreen) {
            throw "目标密码框当前不在可视区域内：$AutomationId"
        }

        # 通过 UIA Action::Focus 在 GPUI 窗口内部聚焦密码框（无需抢占宿主机前台窗口）
        $editEl.SetFocus()
        $isWindowForeground = ([CstDesktopSecretWin32]::GetForegroundWindow() -eq $hwnd)
        if ($isWindowForeground) {
            $focused = $false
            for ($attempt = 0; $attempt -lt 30; $attempt++) {
                if ($editEl.Current.HasKeyboardFocus) {
                    $focused = $true
                    break
                }
                Start-Sleep -Milliseconds 20
            }
            if (-not $focused) {
                throw "前台窗口内的目标密码框未能获取键盘焦点：$AutomationId"
            }
        }

        # 等待重绘帧完成 GPUI ElementInputHandler 挂载
        Start-Sleep -Milliseconds 60

        if (-not [CstDesktopSecretWin32]::SendVirtualKeyTap($hwnd, [CstDesktopSecretWin32]::VK_END)) {
            throw "向窗口投递行尾定位消息超时：$AutomationId"
        }
        for ($clearIdx = 0; $clearIdx -lt 256; $clearIdx++) {
            if (-not [CstDesktopSecretWin32]::IsWindow($hwnd) -or (-not $editEl.Current.IsEnabled) -or $editEl.Current.IsOffscreen) {
                throw "清空旧内容期间控件失效，已中止：$AutomationId"
            }
            if (([CstDesktopSecretWin32]::GetForegroundWindow() -eq $hwnd) -and (-not $editEl.Current.HasKeyboardFocus)) {
                throw "清空旧内容期间焦点转移，已中止：$AutomationId"
            }
            if (-not [CstDesktopSecretWin32]::SendVirtualKeyTap($hwnd, [CstDesktopSecretWin32]::VK_BACK)) {
                throw "向密码框投递退格消息超时：$AutomationId"
            }
        }

        for ($idx = 0; $idx -lt $chars.Length; $idx++) {
            if (-not [CstDesktopSecretWin32]::IsWindow($hwnd) -or (-not $editEl.Current.IsEnabled) -or $editEl.Current.IsOffscreen) {
                throw "逐字符投递期间控件失效，已在第 $idx 个字符处中止：$AutomationId"
            }
            if (([CstDesktopSecretWin32]::GetForegroundWindow() -eq $hwnd) -and (-not $editEl.Current.HasKeyboardFocus)) {
                throw "逐字符投递期间焦点转移，已在第 $idx 个字符处中止：$AutomationId"
            }
            $codeUnit = [uint16]$chars[$idx]
            if (-not [CstDesktopSecretWin32]::SendCharUnit($hwnd, $codeUnit)) {
                throw "向密码框投递字符消息超时：$AutomationId"
            }
        }
    }

    $outputJson = [pscustomobject]@{
        ok = $true
        automationId = $AutomationId
        charCount = $charCount
        redacted = $true
    } | ConvertTo-Json -Compress
    Write-Output $outputJson
} finally {
    if ($null -ne $chars) {
        [Array]::Clear($chars, 0, $chars.Length)
    }
    $secret = $null
}
