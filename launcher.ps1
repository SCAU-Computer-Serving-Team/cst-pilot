$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)

function Test-PilotAdministrator {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    return ([Security.Principal.WindowsPrincipal]$identity).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Test-PilotInteractive {
    return -not [Console]::IsInputRedirected -and -not [Console]::IsOutputRedirected
}

function Get-PilotWindowsTerminal {
    if ($env:LOCALAPPDATA) {
        $alias = Join-Path $env:LOCALAPPDATA 'Microsoft/WindowsApps/wt.exe'
        if (Test-Path -LiteralPath $alias -PathType Leaf) { return $alias }
    }
    $command = Get-Command wt.exe -CommandType Application -ErrorAction SilentlyContinue
    if ($command) { return $command.Source }
    return $null
}

function Start-PilotWindowProcess {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute('PSUseShouldProcessForStateChangingFunctions', '', Justification = 'Internal launcher operation; Windows UAC owns confirmation.')]
    param([string]$FilePath, [string[]]$ArgumentList, [string]$WorkingDirectory, [string]$Verb = 'runas')
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $FilePath
    $start.UseShellExecute = $true
    $start.Verb = $Verb
    $start.WorkingDirectory = $WorkingDirectory
    foreach ($argument in $ArgumentList) { $start.ArgumentList.Add($argument) }
    # Windows receives literal paths and structured arguments; no PowerShell wildcard expansion.
    $process = [Diagnostics.Process]::Start($start)
    if ($process) { $process.Dispose() }
}

function Get-PilotRootException {
    param([Exception]$Exception)
    while ($Exception.InnerException) { $Exception = $Exception.InnerException }
    return $Exception
}

function New-PilotElevationCommand {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute('PSUseShouldProcessForStateChangingFunctions', '', Justification = 'Builds a command string without changing system state.')]
    param([string]$Root, [string]$WorkingDirectory, [string[]]$Arguments)
    $Root = [IO.Path]::TrimEndingDirectorySeparator([IO.Path]::GetFullPath($Root))
    $request = @{ root = $Root; launcher = (Join-Path $Root 'launcher.ps1'); cwd = $WorkingDirectory; arguments = @($Arguments); consoleReady = $true }
    $payload = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes((ConvertTo-Json -InputObject $request -Compress)))
    # Only base64 data enters the command text. Paths and arguments remain JSON data.
    $command = @"
`$request = ConvertFrom-Json ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('$payload')))
. `$request.launcher
if (-not (Test-PilotAdministrator)) { [Console]::Error.WriteLine('[cst-pilot] ERROR: administrator permissions were not granted.'); exit 1 }
Set-Location -LiteralPath `$request.cwd
Invoke-PilotLauncher -Root `$request.root -Arguments ([string[]]`$request.arguments) -ConsoleReady:`$request.consoleReady
exit `$script:PilotExitCode
"@
    return [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($command))
}

function Start-PilotElevated {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute('PSUseShouldProcessForStateChangingFunctions', '', Justification = 'Internal elevation flow; Windows UAC owns confirmation.')]
    param([string]$Root, [string[]]$Arguments)
    $cwd = (Get-Location).ProviderPath
    $command = New-PilotElevationCommand -Root $Root -WorkingDirectory $cwd -Arguments $Arguments
    $pwsh = Join-Path $Root 'pwsh/pwsh.exe'
    $pwshArguments = @('-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-EncodedCommand', $command)
    $terminal = if (Test-PilotInteractive) { Get-PilotWindowsTerminal } else { $null }
    if ($terminal) {
        $terminalArguments = @('-w', 'new', 'new-tab', '--startingDirectory', $cwd, '--title', 'CST Pilot', $pwsh) + $pwshArguments
        try {
            Start-PilotWindowProcess -FilePath $terminal -WorkingDirectory $cwd -Verb RunAs -ArgumentList $terminalArguments
            return
        } catch {
            $cause = Get-PilotRootException -Exception $_.Exception
            if ($cause -is [ComponentModel.Win32Exception] -and $cause.NativeErrorCode -eq 1223) { throw }
            [Console]::Error.WriteLine('[cst-pilot] WARNING: Windows Terminal could not be started; falling back to the console host. ' + $cause.Message)
        }
    }
    Start-PilotWindowProcess -FilePath $pwsh -WorkingDirectory $cwd -Verb RunAs -ArgumentList $pwshArguments
}

function Initialize-PilotEnvironment {
    param([string]$Root)
    $state = Join-Path $Root '.state'
    try {
        foreach ($name in @('tmp', 'cache', 'data')) {
            $directory = Join-Path $state $name
            [IO.Directory]::CreateDirectory($directory) | Out-Null
            [IO.File]::Copy((Join-Path $Root 'pi.cmd'), (Join-Path $directory '.write-probe'), $true)
        }
    } catch {
        throw 'cannot create or write state directories. The media may be write-protected, read-only or full.'
    }
    # Portable WizTree display state is rebuilt for the current machine; retain its backup.
    $wiztreeConfig = Join-Path $Root 'wiztree/WizTree3.ini'
    if (Test-Path -LiteralPath $wiztreeConfig) {
        Move-Item -LiteralPath $wiztreeConfig -Destination (Join-Path $Root 'wiztree/WizTree3.ini.bad') -Force -ErrorAction SilentlyContinue
    }
    $env:PI_CODING_AGENT_DIR = Join-Path $Root 'agent/home'
    $env:PI_OFFLINE = '1'
    $env:POWERSHELL_TELEMETRY_OPTOUT = '1'
    $env:POWERSHELL_UPDATECHECK = 'Off'
    $env:TMP = Join-Path $state 'tmp'
    $env:TEMP = $env:TMP
    $env:XDG_CACHE_HOME = Join-Path $state 'cache'
    $env:XDG_DATA_HOME = Join-Path $state 'data'
    $env:FFF_FRECENCY_DB = Join-Path $env:XDG_DATA_HOME 'fff-frecency.mdb'
    $env:FFF_HISTORY_DB = Join-Path $env:XDG_DATA_HOME 'fff-history.mdb'
    $env:PSModuleAnalysisCachePath = Join-Path $env:XDG_CACHE_HOME 'PSModuleAnalysisCache.txt'
    # Only the kit's saved credentials and fixed endpoints may reach Pi and its extensions.
    foreach ($name in @(
        'COPILOT_GITHUB_TOKEN',
        'ANTHROPIC_AUTH_TOKEN',
        'ANTHROPIC_OAUTH_TOKEN',
        'ANTHROPIC_API_KEY',
        'ANT_LING_API_KEY',
        'QWEN_TOKEN_PLAN_API_KEY',
        'QWEN_TOKEN_PLAN_CN_API_KEY',
        'OPENAI_API_KEY',
        'AZURE_OPENAI_API_KEY',
        'NVIDIA_API_KEY',
        'DEEPSEEK_API_KEY',
        'GEMINI_API_KEY',
        'GOOGLE_CLOUD_API_KEY',
        'GOOGLE_APPLICATION_CREDENTIALS',
        'GOOGLE_CLOUD_PROJECT',
        'GCLOUD_PROJECT',
        'GOOGLE_CLOUD_LOCATION',
        'GROQ_API_KEY',
        'CEREBRAS_API_KEY',
        'XAI_API_KEY',
        'RADIUS_API_KEY',
        'OPENROUTER_API_KEY',
        'AI_GATEWAY_API_KEY',
        'ZAI_API_KEY',
        'ZAI_CODING_CN_API_KEY',
        'MISTRAL_API_KEY',
        'MINIMAX_API_KEY',
        'MINIMAX_CN_API_KEY',
        'MOONSHOT_API_KEY',
        'HF_TOKEN',
        'FIREWORKS_API_KEY',
        'TOGETHER_API_KEY',
        'BASETEN_API_KEY',
        'OPENCODE_API_KEY',
        'KIMI_API_KEY',
        'META_API_KEY',
        'CLOUDFLARE_API_KEY',
        'XIAOMI_API_KEY',
        'XIAOMI_TOKEN_PLAN_CN_API_KEY',
        'XIAOMI_TOKEN_PLAN_AMS_API_KEY',
        'XIAOMI_TOKEN_PLAN_SGP_API_KEY',
        'AWS_PROFILE',
        'AWS_ACCESS_KEY_ID',
        'AWS_SECRET_ACCESS_KEY',
        'AWS_BEARER_TOKEN_BEDROCK',
        'AWS_CONTAINER_CREDENTIALS_RELATIVE_URI',
        'AWS_CONTAINER_CREDENTIALS_FULL_URI',
        'AWS_WEB_IDENTITY_TOKEN_FILE',
        'CSTOA_OA_HOST',
        'CST_PILOT_PWSH',
        'CST_WEB_PORT',
        'CST_WEB_NO_OPEN'
    )) {
        Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
    }
    $paths = @((Join-Path $Root 'pwsh'), (Join-Path $Root 'node'))
    if ($env:PI_INHERIT_HOST_PATH) {
        $paths += $env:PATH
    } else {
        $paths += @((Join-Path $env:WINDIR 'System32'), $env:WINDIR, (Join-Path $env:WINDIR 'System32/Wbem'), (Join-Path $env:WINDIR 'System32/WindowsPowerShell/v1.0'))
    }
    $env:PATH = $paths -join ';'
    $env:PYTHONUTF8 = '1'
    $env:PYTHONIOENCODING = 'utf-8'
}

function Get-PilotRuntime {
    param([string]$Root)
    $arguments = @('--no-skills', '--skill', (Join-Path $Root 'agent/home/skills'), '--no-context-files')
    $binary = Join-Path $Root 'agent/.runtime/prx.bin'
    if (Test-Path -LiteralPath $binary -PathType Leaf) {
        $directory = Get-Item -LiteralPath (Join-Path $Root 'agent/.runtime') -Force
        try { $directory.Attributes = $directory.Attributes -bor [IO.FileAttributes]::Hidden } catch { Write-Verbose 'Runtime directory attributes could not be changed; continuing.' }
        return @{ file = $binary; arguments = $arguments }
    }
    return @{
        file = (Join-Path $Root 'node/node.exe')
        arguments = @((Join-Path $Root 'agent/node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js')) + $arguments
    }
}

function Invoke-PilotRuntime {
    param([string]$Root, [string[]]$Arguments)
    $runtime = Get-PilotRuntime -Root $Root
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $runtime.file
    $start.UseShellExecute = $false
    $start.WorkingDirectory = (Get-Location).ProviderPath
    foreach ($argument in @($runtime.arguments) + @($Arguments)) { $start.ArgumentList.Add([string]$argument) }
    # Direct process creation supports the official renamed .bin and preserves console/RPC handles.
    $process = [Diagnostics.Process]::Start($start)
    try {
        $process.WaitForExit()
        $script:PilotExitCode = $process.ExitCode
    } finally { $process.Dispose() }
}

function Invoke-PilotLauncher {
    param([string]$Root, [string[]]$Arguments, [switch]$ConsoleReady)
    $script:PilotExitCode = 1
    $administrator = Test-PilotAdministrator
    $interactive = Test-PilotInteractive
    if (-not $administrator -and -not $interactive) {
        [Console]::Error.WriteLine('[cst-pilot] ERROR: piped execution requires an administrator terminal. CST Pilot was not started.')
        return
    }
    # Pipe-driven RPC/print runs retain their handles; existing elevated WT sessions stay in place.
    $moveToTerminal = $administrator -and -not $ConsoleReady -and $interactive -and -not $env:WT_SESSION -and (Get-PilotWindowsTerminal)
    if (-not $administrator -or $moveToTerminal) {
        try {
            Start-PilotElevated -Root $Root -Arguments $Arguments
            $script:PilotExitCode = 0
        } catch {
            $cause = Get-PilotRootException -Exception $_.Exception
            if ($cause -is [ComponentModel.Win32Exception] -and $cause.NativeErrorCode -eq 1223) {
                [Console]::Error.WriteLine('[cst-pilot] ERROR: administrator authorization was cancelled. CST Pilot was not started.')
                $script:PilotExitCode = 1223
            } else {
                [Console]::Error.WriteLine('[cst-pilot] ERROR: cannot request administrator authorization. ' + $cause.Message)
            }
        }
        return
    }
    try {
        Initialize-PilotEnvironment -Root $Root
        Invoke-PilotRuntime -Root $Root -Arguments $Arguments
    } catch {
        [Console]::Error.WriteLine('[cst-pilot] ERROR: ' + $_.Exception.Message)
        $script:PilotExitCode = 1
    }
}

# Dot sourcing exposes functions to isolated tests without starting a process or requesting UAC.
if ($MyInvocation.InvocationName -ne '.') {
    Invoke-PilotLauncher -Root $PSScriptRoot -Arguments ([string[]]$args)
    exit $script:PilotExitCode
}
