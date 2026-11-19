[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$OutputDirectory,
    [Parameter(Mandatory)][string]$WorkDirectory,
    [string]$Dotnet = 'dotnet'
)
$ErrorActionPreference = 'Stop'
$work = [IO.Path]::GetFullPath($WorkDirectory)
$output = [IO.Path]::GetFullPath($OutputDirectory)
$project = Join-Path $PSScriptRoot 'CstLhmHost.csproj'
New-Item -ItemType Directory -Force $work | Out-Null
$env:DOTNET_CLI_TELEMETRY_OPTOUT = '1'
$env:DOTNET_SKIP_FIRST_TIME_EXPERIENCE = '1'
$env:DOTNET_GENERATE_ASPNET_CERTIFICATE = 'false'
$env:DOTNET_ADD_GLOBAL_TOOLS_TO_PATH = 'false'
$env:DOTNET_NOLOGO = '1'
$env:DOTNET_CLI_HOME = Join-Path $work 'cli'
$env:NUGET_PACKAGES = Join-Path $work 'nuget'
$stage = Join-Path $work 'publish'
& $Dotnet publish $project -c Release -r win-x64 --self-contained true --disable-build-servers "-p:BaseIntermediateOutputPath=$work/obj/" "-p:BaseOutputPath=$work/bin/" -o $stage
if ($LASTEXITCODE -ne 0) { throw 'LHM 辅助程序构建失败。' }
foreach ($name in @('cst-lhm-host.exe', 'cst-lhm-host.dll', 'cst-lhm-host.runtimeconfig.json', 'hostfxr.dll', 'hostpolicy.dll', 'coreclr.dll')) {
    if (!(Test-Path (Join-Path $stage $name))) { throw "缺少自包含运行文件：$name" }
}
$runtime = Get-Content (Join-Path $stage 'cst-lhm-host.runtimeconfig.json') -Raw | ConvertFrom-Json
if (!$runtime.runtimeOptions.includedFrameworks -or $runtime.runtimeOptions.framework) { throw '辅助程序必须携带 .NET 运行时。' }
$runtimePackage = Join-Path $env:NUGET_PACKAGES 'microsoft.netcore.app.runtime.win-x64/10.0.14'
foreach ($name in @('LICENSE.TXT', 'THIRD-PARTY-NOTICES.TXT')) {
    $source = Join-Path $runtimePackage $name
    if (!(Test-Path $source)) { throw "缺少 .NET 运行时许可：$name" }
    Copy-Item $source (Join-Path $stage "dotnet-$name") -Force
}
New-Item -ItemType Directory -Force $output | Out-Null
Copy-Item (Join-Path $stage '*') $output -Recurse -Force
Write-Output "LHM 辅助程序已发布至 $output"
