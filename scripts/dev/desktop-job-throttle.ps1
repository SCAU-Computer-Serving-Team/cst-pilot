#Requires -Version 7.0
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter()]
    [ValidateRange(0, [int]::MaxValue)]
    [int]$AttachPid = 0,

    [Parameter()]
    [ValidateRange(1, 64)]
    [int]$MaxLogicalCores = 2,

    [Parameter()]
    [ValidateRange(5, 100)]
    [int]$CpuHardCapPercent = 25,

    # Time to keep this script open. Windows keeps the limits until the attached
    # test process exits, even after this script closes its job handle.
    [Parameter()]
    [ValidateRange(1, 600)]
    [int]$HoldSeconds = 10
)

$ErrorActionPreference = 'Stop'

if (-not ('CstDesktopJobThrottle' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;

public sealed class CstDesktopJobThrottle : IDisposable
{
    private const int JobObjectBasicLimitInformation = 2;
    private const int JobObjectCpuRateControlInformation = 15;
    private const uint JOB_OBJECT_LIMIT_AFFINITY = 0x00000010;
    private const uint JOB_OBJECT_CPU_RATE_CONTROL_ENABLE = 0x00000001;
    private const uint JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP = 0x00000004;
    private const uint PROCESS_SET_QUOTA = 0x0100;
    private const uint PROCESS_TERMINATE = 0x0001;
    private const uint PROCESS_QUERY_LIMITED_INFORMATION = 0x1000;

    [StructLayout(LayoutKind.Sequential)]
    private struct JOBOBJECT_BASIC_LIMIT_INFORMATION
    {
        public long PerProcessUserTimeLimit;
        public long PerJobUserTimeLimit;
        public uint LimitFlags;
        public UIntPtr MinimumWorkingSetSize;
        public UIntPtr MaximumWorkingSetSize;
        public uint ActiveProcessLimit;
        public UIntPtr Affinity;
        public uint PriorityClass;
        public uint SchedulingClass;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct JOBOBJECT_CPU_RATE_CONTROL_INFORMATION
    {
        public uint ControlFlags;
        public uint CpuRate;
    }

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr CreateJobObjectW(IntPtr lpJobAttributes, string lpName);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool SetInformationJobObject(
        IntPtr hJob, int infoClass, IntPtr lpJobObjectInfo, uint cbJobObjectInfoLength);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool AssignProcessToJobObject(IntPtr hJob, IntPtr hProcess);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr OpenProcess(uint dwDesiredAccess, bool bInheritHandle, uint dwProcessId);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool GetProcessAffinityMask(
        IntPtr hProcess, out UIntPtr processMask, out UIntPtr systemMask);

    [DllImport("kernel32.dll")]
    private static extern uint GetActiveProcessorCount(ushort groupNumber);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool CloseHandle(IntPtr hObject);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern uint GetGuiResources(IntPtr hProcess, uint uiFlags);

    private IntPtr _jobHandle;

    private CstDesktopJobThrottle(IntPtr handle)
    {
        _jobHandle = handle;
    }

    public static CstDesktopJobThrottle CreateAndAssign(int processId, int maxLogicalCores, int cpuHardCapPercent)
    {
        IntPtr job = CreateJobObjectW(IntPtr.Zero, null);
        if (job == IntPtr.Zero)
        {
            throw new Win32Exception(Marshal.GetLastWin32Error(), "CreateJobObjectW failed");
        }

        IntPtr hProc = IntPtr.Zero;
        try
        {
            hProc = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
                false, (uint)processId);
            if (hProc == IntPtr.Zero)
            {
                throw new Win32Exception(Marshal.GetLastWin32Error(), "OpenProcess failed for Job assignment");
            }
            if (!GetProcessAffinityMask(hProc, out UIntPtr processMask, out _))
            {
                throw new Win32Exception(Marshal.GetLastWin32Error(), "GetProcessAffinityMask failed");
            }

            // Use CPUs allowed by the target's current affinity, not necessarily CPU 0 and 1.
            ulong allowed = processMask.ToUInt64();
            ulong mask = 0;
            int cores = 0;
            for (int bit = 0; bit < UIntPtr.Size * 8 && cores < maxLogicalCores; bit++)
            {
                ulong candidate = 1UL << bit;
                if ((allowed & candidate) != 0)
                {
                    mask |= candidate;
                    cores++;
                }
            }
            if (cores == 0)
            {
                throw new InvalidOperationException("No single-group process affinity mask is available");
            }
            var basic = new JOBOBJECT_BASIC_LIMIT_INFORMATION
            {
                LimitFlags = JOB_OBJECT_LIMIT_AFFINITY,
                Affinity = new UIntPtr(mask)
            };
            IntPtr basicPtr = Marshal.AllocHGlobal(Marshal.SizeOf<JOBOBJECT_BASIC_LIMIT_INFORMATION>());
            try
            {
                Marshal.StructureToPtr(basic, basicPtr, false);
                if (!SetInformationJobObject(job, JobObjectBasicLimitInformation, basicPtr,
                    (uint)Marshal.SizeOf<JOBOBJECT_BASIC_LIMIT_INFORMATION>()))
                {
                    throw new Win32Exception(Marshal.GetLastWin32Error(), "SetInformationJobObject(BasicLimit) failed");
                }
            }
            finally
            {
                Marshal.FreeHGlobal(basicPtr);
            }

            int clampedPct = Math.Max(1, Math.Min(100, cpuHardCapPercent));
            // For a top-level job, CpuRate is relative to all active logical CPUs.
            // An enclosing rate-controlled job can further constrain this cap.
            uint systemCores = GetActiveProcessorCount(0xFFFF); // ALL_PROCESSOR_GROUPS
            if (systemCores == 0)
            {
                throw new InvalidOperationException("No active logical processors reported");
            }
            double effectiveSystemPct = ((double)cores * clampedPct) / systemCores;
            uint cpuRate = (uint)Math.Max(1, Math.Min(10000, (int)Math.Round(effectiveSystemPct * 100.0)));
            var rate = new JOBOBJECT_CPU_RATE_CONTROL_INFORMATION
            {
                ControlFlags = JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP,
                CpuRate = cpuRate
            };
            IntPtr ratePtr = Marshal.AllocHGlobal(Marshal.SizeOf<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>());
            try
            {
                Marshal.StructureToPtr(rate, ratePtr, false);
                if (!SetInformationJobObject(job, JobObjectCpuRateControlInformation, ratePtr,
                    (uint)Marshal.SizeOf<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>()))
                {
                    throw new Win32Exception(Marshal.GetLastWin32Error(), "SetInformationJobObject(CpuRateControl) failed");
                }
            }
            finally
            {
                Marshal.FreeHGlobal(ratePtr);
            }

            var throttle = new CstDesktopJobThrottle(job);
            if (!AssignProcessToJobObject(job, hProc))
            {
                throw new Win32Exception(Marshal.GetLastWin32Error(), "AssignProcessToJobObject failed");
            }
            return throttle;
        }
        catch
        {
            CloseHandle(job);
            throw;
        }
        finally
        {
            if (hProc != IntPtr.Zero)
            {
                CloseHandle(hProc);
            }
        }
    }

    public static uint[] QueryGuiHandles(int processId)
    {
        IntPtr hProc = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, (uint)processId);
        if (hProc == IntPtr.Zero)
        {
            return new uint[] { 0, 0 };
        }
        try
        {
            uint gdi = GetGuiResources(hProc, 0);
            uint user = GetGuiResources(hProc, 1);
            return new uint[] { gdi, user };
        }
        finally
        {
            CloseHandle(hProc);
        }
    }

    public void Dispose()
    {
        if (_jobHandle != IntPtr.Zero)
        {
            CloseHandle(_jobHandle);
            _jobHandle = IntPtr.Zero;
        }
    }
}
'@
}

function Invoke-DesktopJobThrottle {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [int]$ProcessId,
        [int]$Cores = 2,
        [int]$CpuCapPercent = 25
    )
    return [CstDesktopJobThrottle]::CreateAndAssign($ProcessId, $Cores, $CpuCapPercent)
}

function Get-ProcessResourceSample {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [int]$ProcessId,
        [string]$Role = 'desktop',
        [string]$Phase = ''
    )
    $proc = Get-Process -Id $ProcessId -ErrorAction SilentlyContinue
    if (-not $proc) {
        return $null
    }
    try {
        $gui = [CstDesktopJobThrottle]::QueryGuiHandles($ProcessId)
        return [pscustomobject]@{
            timestamp = (Get-Date).ToString('o')
            phase = $Phase
            role = $Role
            pid = $ProcessId
            cpuMs = [Math]::Round($proc.TotalProcessorTime.TotalMilliseconds, 2)
            privateMb = [Math]::Round($proc.PrivateMemorySize64 / 1MB, 2)
            workingSetMb = [Math]::Round($proc.WorkingSet64 / 1MB, 2)
            handles = $proc.HandleCount
            gdiObjects = $gui[0]
            userObjects = $gui[1]
        }
    } finally {
        $proc.Dispose()
    }
}

if ($AttachPid -gt 0) {
    Write-Warning 'Use only with isolated test processes. Job limits remain until the process exits.'
    $throttle = Invoke-DesktopJobThrottle -ProcessId $AttachPid -Cores $MaxLogicalCores -CpuCapPercent $CpuHardCapPercent
    try {
        $result = [pscustomobject]@{
            pid = $AttachPid
            maxLogicalCores = $MaxLogicalCores
            cpuHardCapPercent = $CpuHardCapPercent
            holdSeconds = $HoldSeconds
            status = 'throttled'
        }
        Write-Output ($result | ConvertTo-Json -Compress)
        Start-Sleep -Seconds $HoldSeconds
    } finally {
        $throttle.Dispose()
    }
}
