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

        try
        {
            int cores = Math.Max(1, Math.Min(maxLogicalCores, Environment.ProcessorCount));
            ulong mask = cores >= 64 ? ulong.MaxValue : ((1UL << cores) - 1UL);
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
            // Windows JOBOBJECT_CPU_RATE_CONTROL_INFORMATION.CpuRate is relative to ALL logical cores
            // (10000 = 100% of total machine CPU). Scale by (cores / ProcessorCount) so HardCap truly
            // constrains the assigned core slice on high-core-count developer machines.
            double effectiveSystemPct = ((double)cores * clampedPct) / Math.Max(1, Environment.ProcessorCount);
            uint cpuRate = (uint)Math.Max(100, Math.Min(10000, (int)Math.Round(effectiveSystemPct * 100.0)));
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

            IntPtr hProc = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, (uint)processId);
            if (hProc == IntPtr.Zero)
            {
                throw new Win32Exception(Marshal.GetLastWin32Error(), "OpenProcess failed for Job assignment");
            }
            try
            {
                if (!AssignProcessToJobObject(job, hProc))
                {
                    throw new Win32Exception(Marshal.GetLastWin32Error(), "AssignProcessToJobObject failed");
                }
            }
            finally
            {
                CloseHandle(hProc);
            }

            return new CstDesktopJobThrottle(job);
        }
        catch
        {
            CloseHandle(job);
            throw;
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
}

if ($AttachPid -gt 0) {
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
