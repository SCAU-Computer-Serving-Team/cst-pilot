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
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Threading;

public static class CstDesktopRecordWin32
{
    public const uint PW_RENDERFULLCONTENT = 0x00000002;
    public const uint BI_RGB = 0;
    public const uint DIB_RGB_COLORS = 0;

    [StructLayout(LayoutKind.Sequential)]
    public struct RECT
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct BITMAPINFOHEADER
    {
        public uint biSize;
        public int biWidth;
        public int biHeight;
        public ushort biPlanes;
        public ushort biBitCount;
        public uint biCompression;
        public uint biSizeImage;
        public int biXPelsPerMeter;
        public int biYPelsPerMeter;
        public uint biClrUsed;
        public uint biClrImportant;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct BITMAPINFO
    {
        public BITMAPINFOHEADER bmiHeader;
        public uint bmiColors;
    }

    [DllImport("user32.dll", ExactSpelling = true)]
    public static extern IntPtr SetProcessDpiAwarenessContext(IntPtr value);

    [DllImport("user32.dll", ExactSpelling = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool IsWindow(IntPtr hWnd);

    [DllImport("user32.dll", ExactSpelling = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool GetWindowRect(IntPtr hWnd, out RECT lpRect);

    [DllImport("user32.dll", ExactSpelling = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool PrintWindow(IntPtr hWnd, IntPtr hdcBlt, uint nFlags);

    [DllImport("user32.dll", ExactSpelling = true)]
    public static extern IntPtr GetDC(IntPtr hWnd);

    [DllImport("user32.dll", ExactSpelling = true)]
    public static extern int ReleaseDC(IntPtr hWnd, IntPtr hDC);

    [DllImport("gdi32.dll", ExactSpelling = true)]
    public static extern IntPtr CreateCompatibleDC(IntPtr hdc);

    [DllImport("gdi32.dll", ExactSpelling = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool DeleteDC(IntPtr hdc);

    [DllImport("gdi32.dll", ExactSpelling = true)]
    public static extern IntPtr CreateDIBSection(
        IntPtr hdc,
        ref BITMAPINFO pbmi,
        uint usage,
        out IntPtr ppvBits,
        IntPtr hSection,
        uint offset);

    [DllImport("gdi32.dll", ExactSpelling = true)]
    public static extern IntPtr SelectObject(IntPtr hdc, IntPtr h);

    [DllImport("gdi32.dll", ExactSpelling = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool DeleteObject(IntPtr ho);

    [DllImport("user32.dll", ExactSpelling = true)]
    public static extern IntPtr OpenInputDesktop(uint dwFlags, [MarshalAs(UnmanagedType.Bool)] bool fInherit, uint dwDesiredAccess);

    [DllImport("user32.dll", ExactSpelling = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool CloseDesktop(IntPtr hDesktop);

    public static bool HasInteractiveInputDesktop()
    {
        IntPtr desk = OpenInputDesktop(0, false, 0x0001);
        if (desk == IntPtr.Zero)
        {
            return false;
        }
        CloseDesktop(desk);
        return true;
    }

    private static ulong ComputeSampleHash(byte[] buffer)
    {
        ulong hash = 14695981039346656037UL;
        int step = Math.Max(64, buffer.Length / 4096);
        for (int i = 0; i < buffer.Length; i += step)
        {
            hash ^= buffer[i];
            hash *= 1099511628211UL;
        }
        return hash;
    }

    public static long[] RecordPrintWindowToFfmpeg(
        IntPtr hWnd,
        string ffmpegExe,
        string outputMp4,
        int durationSec,
        int fps,
        int maxEdge)
    {
        try { SetProcessDpiAwarenessContext(new IntPtr(-4)); } catch { }

        if (!IsWindow(hWnd))
        {
            throw new InvalidOperationException("Target HWND is invalid.");
        }

        RECT rc;
        if (!GetWindowRect(hWnd, out rc))
        {
            throw new InvalidOperationException("GetWindowRect failed.");
        }

        int srcW = Math.Max(2, (rc.Right - rc.Left) & ~1);
        int srcH = Math.Max(2, (rc.Bottom - rc.Top) & ~1);
        int outW = srcW;
        int outH = srcH;
        if (maxEdge > 0 && (srcW > maxEdge || srcH > maxEdge))
        {
            double scale = (double)maxEdge / Math.Max(srcW, srcH);
            outW = Math.Max(2, ((int)Math.Round(srcW * scale)) & ~1);
            outH = Math.Max(2, ((int)Math.Round(srcH * scale)) & ~1);
        }

        BITMAPINFO bmi = new BITMAPINFO();
        bmi.bmiHeader.biSize = (uint)Marshal.SizeOf(typeof(BITMAPINFOHEADER));
        bmi.bmiHeader.biWidth = srcW;
        bmi.bmiHeader.biHeight = -srcH;
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB;

        IntPtr screenDc = GetDC(IntPtr.Zero);
        IntPtr memDc = CreateCompatibleDC(screenDc);
        ReleaseDC(IntPtr.Zero, screenDc);

        IntPtr bitsPtr;
        IntPtr dib = CreateDIBSection(memDc, ref bmi, DIB_RGB_COLORS, out bitsPtr, IntPtr.Zero, 0);
        if (dib == IntPtr.Zero)
        {
            DeleteDC(memDc);
            throw new InvalidOperationException("CreateDIBSection failed.");
        }
        IntPtr oldObj = SelectObject(memDc, dib);

        string vfArg = (outW != srcW || outH != srcH)
            ? string.Format("-vf scale={0}:{1} ", outW, outH)
            : string.Empty;
        string ffArgs = string.Format(
            "-loglevel error -f rawvideo -pix_fmt bgra -s {0}x{1} -r {2} -i pipe:0 {3}-c:v libx264 -preset ultrafast -crf 22 -pix_fmt yuv420p -movflags +faststart -y \"{4}\"",
            srcW, srcH, fps, vfArg, outputMp4);

        ProcessStartInfo psi = new ProcessStartInfo
        {
            FileName = ffmpegExe,
            Arguments = ffArgs,
            UseShellExecute = false,
            RedirectStandardInput = true,
            RedirectStandardError = true,
            CreateNoWindow = true
        };

        int totalFrames = Math.Max(1, durationSec * fps);
        List<long> changedTimestampsMs = new List<long>();
        ulong lastHash = 0;
        int frameBytes = srcW * srcH * 4;
        byte[] rawBuf = new byte[frameBytes];

        try
        {
            using (Process ff = Process.Start(psi))
            {
                Stream stdin = ff.StandardInput.BaseStream;
                Stopwatch sw = Stopwatch.StartNew();
                double frameIntervalMs = 1000.0 / fps;

                for (int idx = 0; idx < totalFrames; idx++)
                {
                    if (!IsWindow(hWnd))
                    {
                        break;
                    }
                    long captureMs = sw.ElapsedMilliseconds;
                    PrintWindow(hWnd, memDc, PW_RENDERFULLCONTENT);
                    Marshal.Copy(bitsPtr, rawBuf, 0, frameBytes);
                    stdin.Write(rawBuf, 0, frameBytes);

                    ulong h = ComputeSampleHash(rawBuf);
                    if (idx == 0 || h != lastHash)
                    {
                        lastHash = h;
                        changedTimestampsMs.Add(captureMs);
                    }

                    double targetNextMs = (idx + 1) * frameIntervalMs;
                    double sleepMs = targetNextMs - sw.Elapsed.TotalMilliseconds;
                    if (sleepMs > 1.0)
                    {
                        Thread.Sleep((int)sleepMs);
                    }
                }

                stdin.Flush();
                stdin.Close();
                string errText = ff.StandardError.ReadToEnd();
                ff.WaitForExit();
                if (ff.ExitCode != 0)
                {
                    throw new InvalidOperationException("ffmpeg exited with code " + ff.ExitCode + ": " + errText);
                }
            }

            return changedTimestampsMs.ToArray();
        }
        finally
        {
            SelectObject(memDc, oldObj);
            DeleteObject(dib);
            DeleteDC(memDc);
        }
    }
}
'@
}

$resolvedOut = [IO.Path]::GetFullPath($OutputPath)
$parentDir = [IO.Path]::GetDirectoryName($resolvedOut)
if (-not [string]::IsNullOrEmpty($parentDir)) {
    New-Item -ItemType Directory -Force -Path $parentDir | Out-Null
}

if ([CstDesktopRecordWin32]::HasInteractiveInputDesktop()) {
    $winappArgs = @(
        'ui', 'record', '-w', "$WindowHandle",
        '--duration-sec', "$DurationSec",
        '--fps', "$Fps",
        '--max-edge', "$MaxEdge",
        '--overwrite',
        '--output', $resolvedOut,
        '--json'
    )
    if ($Frames) {
        $winappArgs += @('--frames', '--cursor')
    }
    $winappOut = & winapp @winappArgs 2>$null
    if ($LASTEXITCODE -eq 0 -and (Test-Path $resolvedOut)) {
        Write-Output ($winappOut -join "`n")
        exit 0
    }
}

$ffmpegCmd = Get-Command ffmpeg -ErrorAction SilentlyContinue
if (-not $ffmpegCmd) {
    throw '当前处于非交互沙盒桌面（WGC 不可用），且未找到 ffmpeg 用于 PrintWindow 录屏回退。'
}

$changedMs = [CstDesktopRecordWin32]::RecordPrintWindowToFfmpeg(
    [intptr]$WindowHandle,
    $ffmpegCmd.Source,
    $resolvedOut,
    $DurationSec,
    $Fps,
    $MaxEdge
)

$framesDir = $null
$savedFrames = 0
if ($Frames) {
    $baseNoExt = [IO.Path]::Combine($parentDir, [IO.Path]::GetFileNameWithoutExtension($resolvedOut))
    $framesDir = "${baseNoExt}.frames"
    if (Test-Path $framesDir) {
        Remove-Item -Path $framesDir -Recurse -Force
    }
    New-Item -ItemType Directory -Force -Path $framesDir | Out-Null

    & $ffmpegCmd.Source -loglevel error -i $resolvedOut -vf 'mpdecimate' -vsync vfr "$framesDir\frame-%04d.jpg"
    $jpgFiles = @(Get-ChildItem -Path $framesDir -Filter 'frame-*.jpg' | Sort-Object Name)
    $savedFrames = $jpgFiles.Count
    $ndjsonLines = [Collections.Generic.List[string]]::new()
    for ($i = 0; $i -lt $jpgFiles.Count; $i++) {
        $ms = if ($i -lt $changedMs.Length) { $changedMs[$i] } else { [long]($i * (1000.0 / $Fps)) }
        $lineObj = [pscustomobject]@{
            frame = $i + 1
            file = $jpgFiles[$i].Name
            elapsedMs = $ms
        } | ConvertTo-Json -Compress
        $ndjsonLines.Add($lineObj)
    }
    Set-Content -Path "$framesDir\frames.ndjson" -Value ($ndjsonLines -join "`n") -Encoding utf8NoBOM
}

$summary = [pscustomobject]@{
    ok = $true
    backend = 'printwindow-ffmpeg'
    output = $resolvedOut
    durationSec = $DurationSec
    fps = $Fps
    savedKeyframes = $savedFrames
    framesDir = $framesDir
} | ConvertTo-Json -Compress
Write-Output $summary
