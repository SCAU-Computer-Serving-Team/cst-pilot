#Requires -Version 7.0
[CmdletBinding(DefaultParameterSetName = 'FromEnv', PositionalBinding = $false)]
param(
    [Parameter()]
    [ValidateNotNullOrEmpty()]
    [string]$TargetApp = 'cst-pilot',

    [Parameter()]
    [ValidateRange(1, [int64]::MaxValue)]
    [int64]$WindowHandle,

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
    [DllImport("user32.dll", ExactSpelling = true)]
    public static extern IntPtr GetForegroundWindow();

    [DllImport("user32.dll", ExactSpelling = true)]
    private static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint processId);

    [DllImport("user32.dll", ExactSpelling = true)]
    private static extern IntPtr SendMessageTimeoutW(
        IntPtr hWnd, uint message, UIntPtr wParam, IntPtr lParam,
        uint flags, uint timeoutMs, out UIntPtr result);

    public static uint WindowProcessId(IntPtr hWnd)
    {
        uint processId;
        return GetWindowThreadProcessId(hWnd, out processId) == 0 ? 0 : processId;
    }

    public static bool SendCharUnit(IntPtr hWnd, ushort utf16Unit)
    {
        UIntPtr result;
        return SendMessageTimeoutW(
            hWnd, 0x0102 /* WM_CHAR */, new UIntPtr(utf16Unit), new IntPtr(1),
            0x0002 /* SMTO_ABORTIFHUNG */, 2000, out result) != IntPtr.Zero;
    }
}
'@
}

function Get-DesktopWindowHandle {
    param([string]$AppSelector)

    $processId = 0
    $processes = if ([int]::TryParse($AppSelector, [ref]$processId)) {
        Get-Process -Id $processId -ErrorAction SilentlyContinue
    } else {
        $processName = [WildcardPattern]::Escape(($AppSelector -replace '\.exe$', ''))
        Get-Process -Name $processName -ErrorAction SilentlyContinue
    }
    $windows = @($processes | Where-Object { $_ -and $_.MainWindowHandle -ne [intptr]::Zero })
    if ($windows.Count -ne 1) {
        throw "具有主窗口的目标进程匹配到 $($windows.Count) 个，请用 -WindowHandle 或 PID 精确指定。"
    }
    return $windows[0].MainWindowHandle
}

$hwnd = if ($PSBoundParameters.ContainsKey('WindowHandle')) {
    [intptr]$WindowHandle
} else {
    Get-DesktopWindowHandle -AppSelector $TargetApp
}

$targetPid = [CstDesktopSecretWin32]::WindowProcessId($hwnd)
if ($targetPid -eq 0) {
    throw '指定的窗口句柄无效或已关闭。'
}

$window = [Windows.Automation.AutomationElement]::FromHandle($hwnd)
$condition = [Windows.Automation.PropertyCondition]::new(
    [Windows.Automation.AutomationElement]::AutomationIdProperty, $AutomationId
)
$found = $window.FindAll([Windows.Automation.TreeScope]::Descendants, $condition)
if ($found.Count -ne 1) {
    throw "目标窗口中的 AutomationId '$AutomationId' 匹配到 $($found.Count) 个控件，已中止输入。"
}
$edit = $found[0]

function Assert-DesktopPasswordField {
    param(
        [Windows.Automation.AutomationElement]$Field,
        [uint32]$TargetProcessId
    )

    $current = $Field.Current
    if ($current.ProcessId -ne $TargetProcessId -or
        $current.ControlType -ne [Windows.Automation.ControlType]::Edit -or
        -not $current.IsPassword -or -not $current.IsEnabled -or $current.IsOffscreen) {
        throw "目标不是可见、可用的密码输入框：$($current.AutomationId)"
    }
    $pattern = $null
    if ($Field.TryGetCurrentPattern([Windows.Automation.ValuePattern]::Pattern, [ref]$pattern) -or
        $Field.TryGetCurrentPattern([Windows.Automation.TextPattern]::Pattern, [ref]$pattern)) {
        throw "目标密码框公开了文本读取模式，已中止输入：$($current.AutomationId)"
    }
}

Assert-DesktopPasswordField -Field $edit -TargetProcessId $targetPid
if ([CstDesktopSecretWin32]::GetForegroundWindow() -ne $hwnd) {
    throw '请手动将目标窗口置于前台。脚本无法核实后台密码框的焦点。'
}

$secret = $null
$chars = $null
try {
    if ($FromStdin) {
        if (-not [Console]::IsInputRedirected) {
            throw '请通过管道提供标准输入；脚本不接受会回显的手动输入。'
        }
        $secret = [Console]::In.ReadLine()
    } elseif ($PSCmdlet.ParameterSetName -eq 'FromEnv') {
        $secret = [Environment]::GetEnvironmentVariable($FromEnv)
    } else {
        throw '请指定 -FromEnv 或启用 -FromStdin。'
    }
    if (-not [regex]::IsMatch([string]$secret, '\A[\x21-\x7e]{1,4096}\z')) {
        throw '凭据必须包含 1 至 4096 个不含空格的可打印 ASCII 字符。'
    }
    if ($AutomationId -clike 'bitlocker-pwd-input-*' -and
        -not [regex]::IsMatch($secret, '\A(?:[0-9]{48}|[0-9]{6}(?:-[0-9]{6}){7})\z')) {
        throw 'BitLocker 恢复密码必须由 48 位数字组成，可用连字符分成八组。'
    }
    $chars = $secret.ToCharArray()

    $edit.SetFocus()
    for ($attempt = 0; $attempt -lt 30 -and -not $edit.Current.HasKeyboardFocus; $attempt++) {
        Start-Sleep -Milliseconds 20
    }

    # The password field has no readable value. The caller must clear it before running this script.
    foreach ($char in $chars) {
        Assert-DesktopPasswordField -Field $edit -TargetProcessId $targetPid
        if ([CstDesktopSecretWin32]::WindowProcessId($hwnd) -ne $targetPid -or
            [CstDesktopSecretWin32]::GetForegroundWindow() -ne $hwnd -or
            -not $edit.Current.HasKeyboardFocus) {
            throw "目标密码框失去焦点，已中止。请先清空输入再重试：$AutomationId"
        }
        if (-not [CstDesktopSecretWin32]::SendCharUnit($hwnd, [uint16]$char)) {
            throw "向密码框投递字符失败。请先清空输入再重试：$AutomationId"
        }
    }

    [pscustomobject]@{
        ok = $true
        automationId = $AutomationId
        charCount = $chars.Length
        redacted = $true
    } | ConvertTo-Json -Compress
} finally {
    if ($null -ne $chars) {
        [Array]::Clear($chars, 0, $chars.Length)
    }
    $secret = $null
}
