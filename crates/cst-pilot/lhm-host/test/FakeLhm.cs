namespace LibreHardwareMonitor.Hardware;
public enum HardwareType { Cpu, GpuIntel }
public enum SensorType { Temperature, Fan, Voltage, Load }
public sealed record Sensor(string Name, SensorType SensorType, float? Value);
public sealed class Hardware(string name, HardwareType type)
{
    public string Name => name;
    public HardwareType HardwareType => type;
    public Hardware[] SubHardware => [];
    public Sensor[] Sensors => type == HardwareType.GpuIntel
        ? [new("NaN", SensorType.Temperature, float.NaN), new("Inf", SensorType.Temperature, float.PositiveInfinity), new("Load", SensorType.Load, 45), new("Temp", SensorType.Temperature, 50), new("Null", SensorType.Voltage, null)]
        : Enumerable.Range(0, 220).Select(i => new Sensor("T" + i, SensorType.Temperature, 43.125f)).ToArray();
    public void Update() { if (type == HardwareType.Cpu) throw new InvalidOperationException("模拟部分更新失败"); }
}
public sealed class Computer
{
    public bool IsCpuEnabled { get; set; }
    public bool IsGpuEnabled { get; set; }
    public bool IsMotherboardEnabled { get; set; }
    public bool IsMemoryEnabled { get; set; }
    public bool IsStorageEnabled { get; set; }
    public bool IsControllerEnabled { get; set; }
    public bool IsPsuEnabled { get; set; }
    public bool IsBatteryEnabled { get; set; }
    public Hardware[] Hardware => IsCpuEnabled ? [new("CPU", HardwareType.Cpu), new("GPU", HardwareType.GpuIntel)] : [new("GPU", HardwareType.GpuIntel)];
    public void Open()
    {
        Console.WriteLine("模拟库 stdout");
        if (IsMemoryEnabled || IsStorageEnabled || IsControllerEnabled || IsPsuEnabled || IsBatteryEnabled || !IsGpuEnabled || (IsCpuEnabled != IsMotherboardEnabled)) throw new InvalidOperationException("硬件启用标志错误");
        if (Environment.GetEnvironmentVariable("CST_FAKE_LHM_OPEN_ERROR") == "1") throw new InvalidOperationException("模拟 Open 失败");
    }
    public void Close()
    {
        string? marker = Environment.GetEnvironmentVariable("CST_FAKE_CLOSE_PATH");
        if (marker != null) File.WriteAllText(marker, "closed");
    }
}
