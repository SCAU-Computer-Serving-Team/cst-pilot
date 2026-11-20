#Requires -Version 7.0
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory = $true)]
    [ValidateRange(1, [int64]::MaxValue)]
    [int64]$WindowHandle,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$OutputPath,

    [Parameter()]
    [ValidateRange(1, 120)]
    [int]$DurationSec = 5,

    [Parameter()]
    [ValidateRange(1, 30)]
    [int]$Fps = 15,

    [Parameter()]
    [ValidateRange(0, 4096)]
    [int]$MaxEdge = 1280,

    [Parameter()]
    [switch]$Frames
)

$ErrorActionPreference = 'Stop'

if (-not ('CstDesktopRecordWin32' -as [type])) {
    Add-Type -Path (Join-Path $PSScriptRoot 'desktop-record-win32.cs')
}

function Export-DesktopFrameIndex {
    param(
        [string]$EncoderPath,
        [string]$VideoPath,
        [string]$FrameDirectory
    )
    $PSNativeCommandUseErrorActionPreference = $false
    New-Item -ItemType Directory -Path $FrameDirectory | Out-Null
    $framePattern = Join-Path $FrameDirectory 'frame-%04d.jpg'
    # Use timestamps from the same frames that mpdecimate sends to the JPEG encoder.
    $frameLog = & $EncoderPath -nostdin -n -hide_banner -loglevel info -i $VideoPath -vf 'mpdecimate,showinfo' -fps_mode passthrough $framePattern 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw 'ffmpeg 抽帧失败，未生成帧索引。'
    }

    $frames = @(Get-ChildItem -LiteralPath $FrameDirectory -Filter 'frame-*.jpg' | Sort-Object Name)
    $timestamps = @(
        foreach ($logLine in $frameLog) {
            if ($logLine.ToString() -match '\[Parsed_showinfo_\d+ @ [^\]]+\]\s+n:\s*\d+\s+pts:\s*-?\d+\s+pts_time:\s*([0-9.eE+-]+)') {
                [long][Math]::Round(1000 * [double]::Parse($Matches[1], [Globalization.CultureInfo]::InvariantCulture))
            }
        }
    )
    if ($frames.Count -eq 0 -or $frames.Count -ne $timestamps.Count) {
        throw '变化帧与时间戳数量不一致，未生成帧索引。'
    }

    $frameCount = $frames.Count
    $ndjson = for ($index = 0; $index -lt $frameCount; $index++) {
        [pscustomobject]@{
            frame = $index + 1
            file = $frames[$index].Name
            elapsedMs = $timestamps[$index]
        } | ConvertTo-Json -Compress
    }
    Set-Content -LiteralPath (Join-Path $FrameDirectory 'frames.ndjson') -Value $ndjson -Encoding utf8NoBOM
    return $frameCount
}

$resolvedOut = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputPath)
if ([IO.Path]::GetExtension($resolvedOut) -ne '.mp4') {
    throw '输出路径必须以 .mp4 结尾。'
}
if ($MaxEdge -ne 0 -and $MaxEdge -lt 2) {
    throw '-MaxEdge 必须为 0（原始尺寸）或至少为 2。'
}
if ($Frames -and $MaxEdge -lt 64) {
    throw '启用 -Frames 时，-MaxEdge 必须为 64 至 4096。'
}
if (-not [CstDesktopRecordWin32]::IsWindow([intptr]$WindowHandle)) {
    throw '指定的窗口句柄无效或已关闭。'
}
if ([CstDesktopRecordWin32]::IsIconic([intptr]$WindowHandle)) {
    throw '目标窗口已最小化。请恢复窗口后再录制。'
}
$parentDir = [IO.Path]::GetDirectoryName($resolvedOut)
$framesDir = Join-Path $parentDir ([IO.Path]::GetFileNameWithoutExtension($resolvedOut) + '.frames')
if ((Test-Path -LiteralPath $resolvedOut) -or ($Frames -and (Test-Path -LiteralPath $framesDir))) {
    throw '输出文件或帧目录已存在。请选择新的输出路径。'
}
$null = [IO.Directory]::CreateDirectory($parentDir)

$winappCmd = Get-Command winapp -CommandType Application -ErrorAction SilentlyContinue
if ($winappCmd -and [CstDesktopRecordWin32]::HasInteractiveInputDesktop()) {
    $winappArgs = @(
        'ui', 'record', '-w', "$WindowHandle",
        '--duration-sec', "$DurationSec",
        '--fps', "$Fps",
        '--max-edge', "$MaxEdge",
        '--output', $resolvedOut,
        '--json'
    )
    if ($Frames) {
        $winappArgs += '--frames'
    }

    $PSNativeCommandUseErrorActionPreference = $false
    $winappOut = & $winappCmd.Source @winappArgs
    if ($LASTEXITCODE -eq 0 -and [IO.File]::Exists($resolvedOut) -and
        (-not $Frames -or [IO.File]::Exists((Join-Path $framesDir 'frames.ndjson')))) {
        Write-Output ($winappOut -join "`n")
        return
    }
    if ((Test-Path -LiteralPath $resolvedOut) -or ($Frames -and (Test-Path -LiteralPath $framesDir))) {
        throw 'winapp 未完成录屏，已保留输出文件或帧目录。请检查产物并改用新路径重试。'
    }
}

$ffmpegCmd = Get-Command ffmpeg -CommandType Application -ErrorAction SilentlyContinue
if (-not $ffmpegCmd) {
    throw '当前无法使用 winapp 录屏，且未找到 PrintWindow 录屏所需的 ffmpeg。'
}

[CstDesktopRecordWin32]::RecordToMp4(
    [intptr]$WindowHandle, $ffmpegCmd.Source, $resolvedOut, $DurationSec, $Fps, $MaxEdge
)
if (-not [IO.File]::Exists($resolvedOut) -or [IO.FileInfo]::new($resolvedOut).Length -eq 0) {
    throw 'ffmpeg 未生成有效的视频文件。'
}

$savedFrames = if ($Frames) {
    Export-DesktopFrameIndex -EncoderPath $ffmpegCmd.Source -VideoPath $resolvedOut -FrameDirectory $framesDir
} else {
    0
}

[pscustomobject]@{
    ok = $true
    backend = 'printwindow-ffmpeg'
    output = $resolvedOut
    durationSec = $DurationSec
    fps = $Fps
    savedKeyframes = $savedFrames
    framesDir = if ($Frames) { $framesDir } else { $null }
} | ConvertTo-Json -Compress
