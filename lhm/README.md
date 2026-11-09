# lhm — LibreHardwareMonitorLib 运行时包

`sys sensor` 与 `sys gpu` 使用的数据源 DLL，随发行包提供；开发仓库不包含运行时二进制。

## 内容

| 文件 | 来源 | 版本 |
|---|---|---|
| LibreHardwareMonitorLib.dll | [LibreHardwareMonitor](https://github.com/LibreHardwareMonitor/LibreHardwareMonitor)（MPL-2.0） | 0.9.6 |
| DiskInfoToolkit.dll / RAMSPDToolkit-NDD.dll / HidSharp.dll / System.IO.Ports.dll / System.Management.dll / System.Threading.AccessControl.dll | 上述 NuGet 包的依赖 | 与 LHM 0.9.6 声明的版本一致 |

TFM 选型：pwsh 7.6 运行于 .NET 10，故取 net10.0（HidSharp 取 netstandard2.0）；架构 win-x64。

## Rust 隔离采集

Rust 诊断模块使用 `host/cst-lhm-host.exe` 加载本目录的 DLL，采集过程绑定 Windows Job Object。辅助程序携带 .NET 10 运行时，现场无需安装 .NET；不执行 PowerShell 采集脚本。

构建机需提供 .NET 10 SDK，在仓库根目录执行：

```powershell
./crates/cst-pilot/lhm-host/publish.ps1 -OutputDirectory ./lhm/host -WorkDirectory E:/tmp/<YYYY-MM-DD>/lhm-build
```

发布目录包含程序、运行时及 .NET 许可，整目录随包分发；编译缓存保留在 `WorkDirectory`。辅助程序固定从上级读取 `LibreHardwareMonitorLib.dll` 及其依赖，只接受 `gpu` / `sensor` 两种模式。读取错误按来源返回，传感器仅保留有限值，最多 200 项；库的 `Close()` 在正常返回与异常处理时执行。

模拟 DLL 与真实子进程测试见 [TESTING.md](../doc/TESTING.md#rust-测试环境)。

## 更新方法

1. `https://www.nuget.org/api/v2/package/LibreHardwareMonitorLib/` 下载最新 nupkg（zip）
2. 解出 `runtimes/win-x64/lib/net10.0/LibreHardwareMonitorLib.dll`
3. 按 nuspec 依赖清单更新各依赖 DLL
4. 按[测试规范](../doc/TESTING.md)验证 `sys gpu` 与 `sys sensor`；API 变化时同步 `sys-commands.ts` 的采集模板

## PawnIO 不附带（零安装约束）

CPU 与主板的内核级访问依赖 PawnIO。本项目不附带或安装该驱动：

1. 无驱动时使用用户态可读的传感器。
2. 不保证 CPU 核心温度可读；`frequency` 只提供频率线索，不能单独证明过热。
3. 目标机已有 PawnIO 且权限足够时，LHM 可能返回更多传感器。

PawnIO：https://pawnio.cc

## 保留未启用依赖

`sys-commands.ts` 的传感器模板只开 Cpu/Gpu/Motherboard，存储/内存/控制器关闭时 DiskInfoToolkit、RAMSPDToolkit 等不会被加载。保留它们的目的是：后续开启更多硬件类型时不必重新组包。
