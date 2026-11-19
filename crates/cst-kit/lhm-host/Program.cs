using System.Collections;
using System.Reflection;
using System.Runtime.Loader;
using System.Text.Json;

namespace CstPilot.LhmHost;

internal sealed class LhmContext(string directory) : AssemblyLoadContext
{
    protected override Assembly? Load(AssemblyName name)
    {
        if (string.IsNullOrEmpty(name.Name) || Path.GetFileName(name.Name) != name.Name) return null;
        string file = Path.Combine(directory, name.Name + ".dll");
        return File.Exists(file) ? LoadFromAssemblyPath(file) : null;
    }
    protected override IntPtr LoadUnmanagedDll(string name)
    {
        if (Path.GetFileName(name) != name) return IntPtr.Zero;
        string file = Path.Combine(directory, name.EndsWith(".dll", StringComparison.OrdinalIgnoreCase) ? name : name + ".dll");
        return File.Exists(file) ? LoadUnmanagedDllFromPath(file) : IntPtr.Zero;
    }
}

internal static class Program
{
    private static object? Get(object target, string name) => target.GetType().GetProperty(name)?.GetValue(target);
    private static void Set(object target, string name, bool value)
    {
        PropertyInfo property = target.GetType().GetProperty(name) ?? throw new MissingMemberException(name);
        property.SetValue(target, value);
    }
    private static void Call(object target, string name) => (target.GetType().GetMethod(name, Type.EmptyTypes) ?? throw new MissingMemberException(name)).Invoke(target, null);
    private static IEnumerable<object> Items(object? value) => value is IEnumerable list ? list.Cast<object>() : [];
    private static string Text(object? value) { string text = value?.ToString() ?? ""; return text[..Math.Min(text.Length, 1024)]; }
    private static object Collect(bool gpu)
    {
        string directory = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, ".."));
        string file = Path.Combine(directory, "LibreHardwareMonitorLib.dll");
        var context = new LhmContext(directory);
        Type type = context.LoadFromAssemblyPath(file).GetType("LibreHardwareMonitor.Hardware.Computer", true)!;
        object computer = Activator.CreateInstance(type) ?? throw new InvalidOperationException("LHM Computer 初始化失败");
        var hardware = new List<string>();
        var sensors = new List<object>();
        var errors = new List<object>();
        string[] accepted = gpu ? ["Temperature", "Load", "Clock", "Power", "Fan", "SmallData", "Data"] : ["Temperature", "Fan", "Voltage"];
        void Failed(string source, Exception error) { if (errors.Count < 16) errors.Add(new { source, message = Text((error.InnerException ?? error).Message) }); }
        foreach (string flag in new[] { "IsCpuEnabled", "IsGpuEnabled", "IsMotherboardEnabled", "IsMemoryEnabled", "IsStorageEnabled", "IsControllerEnabled", "IsPsuEnabled", "IsBatteryEnabled" })
            Set(computer, flag, flag == "IsGpuEnabled" || (!gpu && flag is "IsCpuEnabled" or "IsMotherboardEnabled"));
        try
        {
            Call(computer, "Open");
            int visited = 0;
            foreach (object hw in Items(Get(computer, "Hardware")))
            {
                string name = Text(Get(hw, "Name"));
                if (!gpu || Text(Get(hw, "HardwareType")).StartsWith("Gpu", StringComparison.Ordinal)) hardware.Add(name);
                var pending = new Stack<object>();
                pending.Push(hw);
                while (pending.TryPop(out object? current) && sensors.Count < 200)
                {
                    if (++visited > 1024) throw new InvalidOperationException("LHM 硬件节点超过采集上限");
                    try { Call(current, "Update"); } catch (Exception error) { Failed(name, error); }
                    foreach (object sensor in Items(Get(current, "Sensors")))
                    {
                        string sensorType = Text(Get(sensor, "SensorType"));
                        object? raw = Get(sensor, "Value");
                        if (raw is null || !accepted.Contains(sensorType)) continue;
                        double value = Convert.ToDouble(raw, System.Globalization.CultureInfo.InvariantCulture);
                        if (!double.IsFinite(value)) continue;
                        sensors.Add(new { hw = name, name = Text(Get(sensor, "Name")), type = sensorType, value = Math.Round(value, 2) });
                        if (sensors.Count >= 200) break;
                    }
                    foreach (object sub in Items(Get(current, "SubHardware"))) pending.Push(sub);
                }
                if (sensors.Count >= 200 || hardware.Count >= 200) break;
            }
        }
        finally
        {
            try { Call(computer, "Close"); } catch (Exception error) { Failed("Close", error); }
        }
        return new { hardware, sensors, collectionErrors = errors, degraded = errors.Count > 0 };
    }
    public static int Main(string[] args)
    {
        using var writer = new StreamWriter(Console.OpenStandardOutput(), new System.Text.UTF8Encoding(false)) { AutoFlush = true };
        // Library diagnostics must not corrupt the one JSON response.
        Console.SetOut(TextWriter.Null);
        Console.SetError(TextWriter.Null);
        try
        {
            if (args.Length != 1 || args[0] is not ("gpu" or "sensor")) throw new ArgumentException("采集模式必须为 gpu 或 sensor");
            writer.Write(JsonSerializer.Serialize(Collect(args[0] == "gpu")));
        }
        catch (Exception error)
        {
            writer.Write(JsonSerializer.Serialize(new { error = Text((error.InnerException ?? error).Message) }));
        }
        return 0;
    }
}
