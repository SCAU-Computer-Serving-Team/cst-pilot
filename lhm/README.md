# lhm — LibreHardwareMonitorLib 运行时包

`sys sensor` 与 `sys gpu` 使用的数据源 DLL，随发行包提供；开发仓库不包含运行时二进制。

## 内容

| 文件 | 来源 | 版本 |
|---|---|---|
| LibreHardwareMonitorLib.dll | [LibreHardwareMonitor](https://github.com/LibreHardwareMonitor/LibreHardwareMonitor)（MPL-2.0） | 0.9.6 |
| DiskInfoToolkit.dll / RAMSPDToolkit-NDD.dll / HidSharp.dll / System.IO.Ports.dll / System.Management.dll / System.Threading.AccessControl.dll | 上述 NuGet 包的依赖 | 与 LHM 0.9.6 声明的版本一致 |

TFM 选型：pwsh 7.6 运行于 .NET 10，故取 net10.0（HidSharp 取 netstandard2.0）；架构 win-x64。

## 更新方法

1. `https://www.nuget.org/api/v2/package/LibreHardwareMonitorLib/` 下载最新 nupkg（zip）
2. 解出 `runtimes/win-x64/lib/net10.0/LibreHardwareMonitorLib.dll`
3. 按 nuspec 依赖清单更新各依赖 DLL
4. 按[测试指南](../doc/test/README.md)验证 `sys gpu` 与 `sys sensor`；API 变化时同步 `sys-commands.ts` 的采集模板

## PawnIO 不附带（零安装约束）

CPU 与主板的内核级访问依赖 PawnIO。本项目不附带或安装该驱动：

1. 无驱动时使用用户态可读的传感器。
2. 不保证 CPU 核心温度可读；`frequency` 只提供频率线索，不能单独证明过热。
3. 目标机已有 PawnIO 且权限足够时，LHM 可能返回更多传感器。

PawnIO：https://pawnio.cc

## 用不到的依赖为什么也打包

`sys-commands.ts` 的传感器模板只开 Cpu/Gpu/Motherboard，存储/内存/控制器关闭时
DiskInfoToolkit、RAMSPDToolkit 等不会被加载。保留它们是防御性的：
一旦未来开启更多硬件类型，不必重新组包。
