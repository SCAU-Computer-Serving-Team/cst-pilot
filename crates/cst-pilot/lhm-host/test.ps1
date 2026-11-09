[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$WorkDirectory,
    [string]$Dotnet = 'dotnet'
)
$ErrorActionPreference = 'Stop'
$work = [IO.Path]::GetFullPath($WorkDirectory)
$root = Join-Path $work 'kit'
$hostDir = Join-Path $root 'lhm/host'
& (Join-Path $PSScriptRoot 'publish.ps1') -OutputDirectory $hostDir -WorkDirectory (Join-Path $work 'host-build') -Dotnet $Dotnet
& $Dotnet build (Join-Path $PSScriptRoot 'test/FakeLhm.csproj') -c Release --disable-build-servers "-p:BaseIntermediateOutputPath=$work/fake-obj/" "-p:BaseOutputPath=$work/fake-bin/" -o (Join-Path $root 'lhm')
if ($LASTEXITCODE -ne 0) { throw '模拟 LHM DLL 构建失败。' }
$exe = Join-Path $hostDir 'cst-lhm-host.exe'
$env:CST_FAKE_CLOSE_PATH = Join-Path $work 'closed.txt'
try {
    $sensor = (& $exe sensor | ConvertFrom-Json)
    if ($sensor.sensors.Count -ne 200 -or !$sensor.degraded -or $sensor.sensors[0].type -ne 'Temperature') { throw '传感器过滤、上限或部分错误不符。' }
    if (!(Test-Path $env:CST_FAKE_CLOSE_PATH)) { throw '辅助程序未关闭 LHM。' }
    $gpu = (& $exe gpu | ConvertFrom-Json)
    if ($gpu.sensors.Count -ne 2 -or $gpu.sensors[0].type -ne 'Load') { throw 'GPU 模式的字段过滤不符。' }
    Remove-Item $env:CST_FAKE_CLOSE_PATH
    $env:CST_FAKE_LHM_OPEN_ERROR = '1'
    $failed = (& $exe sensor | ConvertFrom-Json)
    if ($failed.error -ne '模拟 Open 失败') { throw 'Open 错误未返回。' }
    if (!(Test-Path $env:CST_FAKE_CLOSE_PATH)) { throw 'Open 失败后未关闭 LHM。' }
    Remove-Item Env:CST_FAKE_LHM_OPEN_ERROR
    $bad = (& $exe invalid | ConvertFrom-Json)
    if (!$bad.error) { throw '模式校验未拒绝非法输入。' }
    $env:CST_TEST_LHM_ROOT = $root
    cargo test -p cst-pilot --test sys_load --locked --offline lhm_host_fixture_is_loaded_through_real_job_object -- --ignored --exact
    if ($LASTEXITCODE -ne 0) { throw 'Job Object 内真实辅助程序测试失败。' }
} finally {
    Remove-Item Env:CST_FAKE_CLOSE_PATH,Env:CST_FAKE_LHM_OPEN_ERROR,Env:CST_TEST_LHM_ROOT -ErrorAction SilentlyContinue
}
Write-Output 'LHM 自包含辅助程序、模拟 DLL 与 Job Object 联调通过。'
