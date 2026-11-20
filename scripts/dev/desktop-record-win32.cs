using System;
using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Runtime.InteropServices;
using System.Threading;

public static class CstDesktopRecordWin32
{
    private const uint PW_RENDERFULLCONTENT = 0x00000002;
    private const uint BI_RGB = 0;
    private const uint DIB_RGB_COLORS = 0;

    [StructLayout(LayoutKind.Sequential)]
    private struct RECT
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct BITMAPINFOHEADER
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
    private struct BITMAPINFO
    {
        public BITMAPINFOHEADER bmiHeader;
        public uint bmiColors;
    }

    [DllImport("user32.dll", ExactSpelling = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetProcessDpiAwarenessContext(IntPtr value);

    [DllImport("user32.dll", ExactSpelling = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool IsWindow(IntPtr hWnd);

    [DllImport("user32.dll", ExactSpelling = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool IsIconic(IntPtr hWnd);

    [DllImport("user32.dll", ExactSpelling = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetWindowRect(IntPtr hWnd, out RECT lpRect);

    [DllImport("user32.dll", ExactSpelling = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool PrintWindow(IntPtr hWnd, IntPtr hdcBlt, uint nFlags);

    [DllImport("gdi32.dll", ExactSpelling = true)]
    private static extern IntPtr CreateCompatibleDC(IntPtr hdc);

    [DllImport("gdi32.dll", ExactSpelling = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool DeleteDC(IntPtr hdc);

    [DllImport("gdi32.dll", ExactSpelling = true)]
    private static extern IntPtr CreateDIBSection(
        IntPtr hdc,
        ref BITMAPINFO pbmi,
        uint usage,
        out IntPtr ppvBits,
        IntPtr hSection,
        uint offset);

    [DllImport("gdi32.dll", ExactSpelling = true)]
    private static extern IntPtr SelectObject(IntPtr hdc, IntPtr h);

    [DllImport("gdi32.dll", ExactSpelling = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool DeleteObject(IntPtr ho);

    [DllImport("user32.dll", ExactSpelling = true)]
    private static extern IntPtr OpenInputDesktop(uint dwFlags, [MarshalAs(UnmanagedType.Bool)] bool fInherit, uint dwDesiredAccess);

    [DllImport("user32.dll", ExactSpelling = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CloseDesktop(IntPtr hDesktop);

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

    public static void RecordToMp4(
        IntPtr hWnd,
        string ffmpegExe,
        string outputMp4,
        int durationSec,
        int fps,
        int maxEdge)
    {
        // DPI_PER_MONITOR_AWARE_V2. The host may already have a process DPI policy.
        SetProcessDpiAwarenessContext(new IntPtr(-4));

        if (!IsWindow(hWnd) || IsIconic(hWnd))
        {
            throw new InvalidOperationException("Target window is closed or minimized.");
        }

        RECT rc;
        if (!GetWindowRect(hWnd, out rc))
        {
            throw new InvalidOperationException("GetWindowRect failed.");
        }

        long width = (long)rc.Right - rc.Left;
        long height = (long)rc.Bottom - rc.Top;
        if (width < 2 || height < 2 || width * (double)height > 16777216)
        {
            throw new InvalidOperationException("Invalid source dimensions or pixel count exceeds 16777216.");
        }
        int srcW = (int)width & ~1;
        int srcH = (int)height & ~1;
        int frameBytes = checked(srcW * srcH * 4);
        byte[] rawBuf = new byte[frameBytes];
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

        IntPtr memDc = CreateCompatibleDC(IntPtr.Zero);
        if (memDc == IntPtr.Zero)
        {
            throw new InvalidOperationException("CreateCompatibleDC failed.");
        }

        IntPtr dib = IntPtr.Zero;
        IntPtr oldObj = IntPtr.Zero;
        try
        {
            IntPtr bitsPtr;
            dib = CreateDIBSection(memDc, ref bmi, DIB_RGB_COLORS, out bitsPtr, IntPtr.Zero, 0);
            if (dib == IntPtr.Zero || bitsPtr == IntPtr.Zero)
            {
                throw new InvalidOperationException("CreateDIBSection failed.");
            }
            oldObj = SelectObject(memDc, dib);
            if (oldObj == IntPtr.Zero)
            {
                throw new InvalidOperationException("SelectObject failed.");
            }

            var encoder = CreateEncoder(ffmpegExe, outputMp4, srcW, srcH, outW, outH, fps);
            using (Process ffmpeg = Process.Start(encoder))
            {
                var stderr = ffmpeg.StandardError.ReadToEndAsync();
                try
                {
                    // Closing stdin tells FFmpeg to write the trailer before exiting.
                    using (Stream stdin = ffmpeg.StandardInput.BaseStream)
                    {
                        WriteFrames(hWnd, memDc, bitsPtr, rawBuf, stdin, durationSec * fps, fps);
                    }
                    if (!ffmpeg.WaitForExit(30000))
                    {
                        throw new InvalidOperationException("ffmpeg did not finish encoding within 30 seconds.");
                    }
                    if (ffmpeg.ExitCode != 0)
                    {
                        throw new InvalidOperationException("ffmpeg exited with code " + ffmpeg.ExitCode + ": " +
                            stderr.GetAwaiter().GetResult());
                    }
                }
                finally
                {
                    if (!ffmpeg.HasExited)
                    {
                        ffmpeg.Kill();
                    }
                }
            }
        }
        finally
        {
            if (oldObj != IntPtr.Zero)
            {
                SelectObject(memDc, oldObj);
            }
            if (dib != IntPtr.Zero)
            {
                DeleteObject(dib);
            }
            DeleteDC(memDc);
        }
    }

    private static ProcessStartInfo CreateEncoder(
        string ffmpegExe, string outputMp4,
        int sourceWidth, int sourceHeight, int outputWidth, int outputHeight, int fps)
    {
        var encoder = new ProcessStartInfo
        {
            FileName = ffmpegExe,
            UseShellExecute = false,
            RedirectStandardInput = true,
            RedirectStandardError = true,
            CreateNoWindow = true
        };
        AddArguments(encoder,
            "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "bgra",
            "-s", FormattableString.Invariant($"{sourceWidth}x{sourceHeight}"),
            "-r", fps.ToString(CultureInfo.InvariantCulture), "-i", "pipe:0");
        if (sourceWidth != outputWidth || sourceHeight != outputHeight)
        {
            AddArguments(encoder, "-vf", FormattableString.Invariant($"scale={outputWidth}:{outputHeight}"));
        }
        AddArguments(encoder, "-c:v", "libx264", "-preset", "ultrafast", "-crf", "22",
            "-pix_fmt", "yuv420p", "-movflags", "+faststart", "-n", outputMp4);
        return encoder;
    }

    private static void AddArguments(ProcessStartInfo process, params string[] arguments)
    {
        foreach (string argument in arguments)
        {
            process.ArgumentList.Add(argument);
        }
    }

    private static void WriteFrames(
        IntPtr hWnd, IntPtr deviceContext, IntPtr pixels,
        byte[] frame, Stream pipe, int frameCount, int fps)
    {
        Stopwatch clock = Stopwatch.StartNew();
        double frameIntervalMs = 1000.0 / fps;
        for (int frameIndex = 0; frameIndex < frameCount; frameIndex++)
        {
            if (!IsWindow(hWnd) || IsIconic(hWnd) ||
                !PrintWindow(hWnd, deviceContext, PW_RENDERFULLCONTENT))
            {
                throw new InvalidOperationException("PrintWindow failed or the target window closed or minimized.");
            }
            Marshal.Copy(pixels, frame, 0, frame.Length);
            pipe.Write(frame, 0, frame.Length);

            double sleepMs = (frameIndex + 1) * frameIntervalMs - clock.Elapsed.TotalMilliseconds;
            if (sleepMs > 1.0)
            {
                Thread.Sleep((int)sleepMs);
            }
        }
    }
}
