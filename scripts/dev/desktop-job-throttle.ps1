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
    [ValidateRange(0, 16384)]
    [int]$MemoryLimitMB = 0,

    [Parameter()]
    [switch]$BelowNormalPriority,

    # Time to keep this script open. Windows keeps the limits until the attached
    # test process exits, even after this script closes its job handle.
    [Parameter()]
    [ValidateRange(1, 600)]
    [int]$HoldSeconds = 10
)

if (-not $IsWindows) { throw 'Job Object throttling requires Windows.' }

if (-not ('CstDesktopJobThrottle' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;

public sealed class CstDesktopJobThrottle : IDisposable
{
    private const int JobObjectExtendedLimitInformation = 9;
    private const int JobObjectCpuRateControlInformation = 15;
    private const uint JOB_OBJECT_LIMIT_AFFINITY = 0x00000010;
    private const uint JOB_OBJECT_LIMIT_PRIORITY_CLASS = 0x00000020;
    private const uint JOB_OBJECT_LIMIT_PROCESS_MEMORY = 0x00000100;
    private const uint BELOW_NORMAL_PRIORITY_CLASS = 0x00004000;
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
    private struct IO_COUNTERS
    {
        public ulong ReadOperationCount;
        public ulong WriteOperationCount;
        public ulong OtherOperationCount;
        public ulong ReadTransferCount;
        public ulong WriteTransferCount;
        public ulong OtherTransferCount;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct JOBOBJECT_EXTENDED_LIMIT_INFORMATION
    {
        public JOBOBJECT_BASIC_LIMIT_INFORMATION BasicLimitInformation;
        public IO_COUNTERS IoInfo;
        public UIntPtr ProcessMemoryLimit;
        public UIntPtr JobMemoryLimit;
        public UIntPtr PeakProcessMemoryUsed;
        public UIntPtr PeakJobMemoryUsed;
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
        IntPtr hJob, int infoClass, in JOBOBJECT_EXTENDED_LIMIT_INFORMATION info, uint size);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool SetInformationJobObject(
        IntPtr hJob, int infoClass, in JOBOBJECT_CPU_RATE_CONTROL_INFORMATION info, uint size);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool AssignProcessToJobObject(IntPtr hJob, IntPtr hProcess);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr OpenProcess(uint dwDesiredAccess, bool bInheritHandle, uint dwProcessId);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool GetProcessAffinityMask(
        IntPtr hProcess, out UIntPtr processMask, out UIntPtr systemMask);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern uint GetActiveProcessorCount(ushort groupNumber);

    [DllImport("kernel32.dll")]
    private static extern ushort GetActiveProcessorGroupCount();

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool CloseHandle(IntPtr hObject);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern uint GetGuiResources(IntPtr hProcess, uint uiFlags);

    private IntPtr _jobHandle;

    public int LogicalCores { get; }
    public uint CpuRate { get; }
    public ulong ProcessCommitLimitBytes { get; }
    public bool BelowNormalPriority { get; }

    private CstDesktopJobThrottle(IntPtr handle, int cores, uint cpuRate, ulong memoryLimit, bool belowNormal)
    {
        _jobHandle = handle;
        LogicalCores = cores;
        CpuRate = cpuRate;
        ProcessCommitLimitBytes = memoryLimit;
        BelowNormalPriority = belowNormal;
    }

    private static Win32Exception LastError(string api)
    {
        int code = Marshal.GetLastWin32Error();
        return new Win32Exception(code, $"{api} failed ({code}): {new Win32Exception(code).Message}");
    }

    public static CstDesktopJobThrottle CreateAndAssign(int processId, int maxLogicalCores, int cpuHardCapPercent)
    {
        return CreateAndAssign(processId, maxLogicalCores, cpuHardCapPercent, 0, false);
    }

    public static CstDesktopJobThrottle CreateAndAssign(
        int processId, int maxLogicalCores, int cpuHardCapPercent, int memoryLimitMb, bool belowNormalPriority)
    {
        if (processId <= 0)
            throw new ArgumentOutOfRangeException(nameof(processId));
        if (maxLogicalCores < 1 || maxLogicalCores > UIntPtr.Size * 8)
            throw new ArgumentOutOfRangeException(nameof(maxLogicalCores));
        if (cpuHardCapPercent < 5 || cpuHardCapPercent > 100)
            throw new ArgumentOutOfRangeException(nameof(cpuHardCapPercent));
        if (memoryLimitMb < 0 || memoryLimitMb > 16384)
            throw new ArgumentOutOfRangeException(nameof(memoryLimitMb));
        ulong memoryLimit = (ulong)memoryLimitMb * 1024UL * 1024UL;
        if (UIntPtr.Size == 4 && memoryLimit > uint.MaxValue)
            throw new ArgumentOutOfRangeException(nameof(memoryLimitMb), "Commit limit does not fit a 32-bit SIZE_T.");
        if (GetActiveProcessorGroupCount() != 1)
            throw new NotSupportedException("Job throttling requires a single active processor group.");

        IntPtr job = CreateJobObjectW(IntPtr.Zero, null);
        if (job == IntPtr.Zero)
        {
            throw LastError("CreateJobObjectW");
        }

        IntPtr hProc = IntPtr.Zero;
        try
        {
            hProc = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
                false, (uint)processId);
            if (hProc == IntPtr.Zero)
            {
                throw LastError("OpenProcess for Job assignment");
            }
            if (!GetProcessAffinityMask(hProc, out UIntPtr processMask, out _))
            {
                throw LastError("GetProcessAffinityMask");
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
            uint limitFlags = JOB_OBJECT_LIMIT_AFFINITY;
            uint priorityClass = 0;
            if (belowNormalPriority)
            {
                limitFlags |= JOB_OBJECT_LIMIT_PRIORITY_CLASS;
                priorityClass = BELOW_NORMAL_PRIORITY_CLASS;
            }
            if (memoryLimit > 0)
                limitFlags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;
            var extended = new JOBOBJECT_EXTENDED_LIMIT_INFORMATION
            {
                BasicLimitInformation = new JOBOBJECT_BASIC_LIMIT_INFORMATION
                {
                    LimitFlags = limitFlags,
                    Affinity = new UIntPtr(mask),
                    PriorityClass = priorityClass
                },
                ProcessMemoryLimit = new UIntPtr(memoryLimit)
            };
            if (!SetInformationJobObject(job, JobObjectExtendedLimitInformation, in extended,
                (uint)Marshal.SizeOf<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()))
            {
                throw LastError("SetInformationJobObject(ExtendedLimit)");
            }
            // For a top-level job, CpuRate is relative to all active logical CPUs.
            // An enclosing rate-controlled job can further constrain this cap.
            uint systemCores = GetActiveProcessorCount(0xFFFF); // ALL_PROCESSOR_GROUPS
            if (systemCores == 0)
            {
                throw LastError("GetActiveProcessorCount");
            }
            uint cpuRate = Math.Max(1U, (uint)cores * (uint)cpuHardCapPercent * 100U / systemCores);
            var rate = new JOBOBJECT_CPU_RATE_CONTROL_INFORMATION
            {
                ControlFlags = JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP,
                CpuRate = cpuRate
            };
            if (!SetInformationJobObject(job, JobObjectCpuRateControlInformation, in rate,
                (uint)Marshal.SizeOf<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>()))
            {
                throw LastError("SetInformationJobObject(CpuRateControl)");
            }

            var throttle = new CstDesktopJobThrottle(job, cores, cpuRate, memoryLimit, belowNormalPriority);
            if (!AssignProcessToJobObject(job, hProc))
            {
                throw LastError("AssignProcessToJobObject");
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

    public static uint?[] QueryGuiHandles(int processId)
    {
        IntPtr hProc = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, (uint)processId);
        if (hProc == IntPtr.Zero)
        {
            return new uint?[] { null, null };
        }
        try
        {
            return new uint?[] { QueryGuiCount(hProc, 0), QueryGuiCount(hProc, 1) };
        }
        finally
        {
            CloseHandle(hProc);
        }
    }

    private static uint? QueryGuiCount(IntPtr process, uint flag)
    {
        uint count = GetGuiResources(process, flag);
        return count == 0 && Marshal.GetLastWin32Error() != 0 ? (uint?)null : count;
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
        [ValidateRange(1, [int]::MaxValue)]
        [int]$ProcessId,
        [ValidateRange(1, 64)]
        [int]$Cores = 2,
        [ValidateRange(5, 100)]
        [int]$CpuCapPercent = 25,
        [ValidateRange(0, 16384)]
        [int]$MemoryLimitMB = 0,
        [switch]$BelowNormalPriority
    )
    return [CstDesktopJobThrottle]::CreateAndAssign(
        $ProcessId, $Cores, $CpuCapPercent, $MemoryLimitMB, [bool]$BelowNormalPriority)
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
    $throttle = Invoke-DesktopJobThrottle -ProcessId $AttachPid -Cores $MaxLogicalCores -CpuCapPercent $CpuHardCapPercent -MemoryLimitMB $MemoryLimitMB -BelowNormalPriority:$BelowNormalPriority
    try {
        $result = [pscustomobject]@{
            pid = $AttachPid
            maxLogicalCores = $MaxLogicalCores
            cpuHardCapPercent = $CpuHardCapPercent
            selectedLogicalCores = $throttle.LogicalCores
            cpuRate = $throttle.CpuRate
            memoryLimitMb = $MemoryLimitMB
            belowNormalPriority = [bool]$BelowNormalPriority
            holdSeconds = $HoldSeconds
            status = 'throttled'
        }
        Write-Output ($result | ConvertTo-Json -Compress)
        Start-Sleep -Seconds $HoldSeconds
    } finally {
        $throttle.Dispose()
    }
}
