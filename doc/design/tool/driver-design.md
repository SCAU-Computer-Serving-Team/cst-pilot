# driver 设计

状态：四个 scope 已实现。需求：[PRD R7](../../PRD.md)；接口：[driver](../../tool/driver.md)。

## 目标与边界

只读盘点设备和驱动，支持蓝牙、网络、音频、显示及外设排查。使用当前系统 WMI/CIM，不安装驱动、不修改设备或服务。支持目标为 Windows 10/11 x64，裁剪系统和缺失数据源须明确降级。

driver 判断设备识别与状态；sys 处理负载和传感器，disk 处理空间和可靠性。IP 配置、连通性、信号强度不纳入本工具。

## 接口选择

采用一个工具、多个 scope：problem 默认入口，core 提供关键硬件概况，external 覆盖外设，find 补充触控板、指纹等特定设备查询。

| 备选 | 未采用原因 |
|---|---|
| 每类设备独立工具 | 数据同源，重复 schema 和说明，增加选择成本 |
| 每次全量快照 | 不必要地枚举并返回大量设备 |
| 仅三个固定场景 | 无自由 shell 时，缺少长尾设备查询入口 |

## 采集结构

[driver.ts](../../../agent/home/extensions/diagnostics/driver.ts) 负责注册和参数 schema；[driver-core.ts](../../../agent/home/extensions/diagnostics/driver-core.ts) 负责校验、命令、采集和返回。collect 函数可供局部脚本直接验证。

| scope | 数据源与筛选 | 理由 |
|---|---|---|
| problem | Win32_PnPEntity，Status=Error/Unknown | 保留状态异常但 errorCode 可能为 0 的设备 |
| core | NetworkAdapter、PnPEntity、VideoController、Service、PnPSignedDriver | 设备状态与蓝牙/音频服务共同排查；版本仅按四类查询 |
| external | PnP 的 USB/BTHENUM/DISPLAY 前缀；USB 或可移动 DiskDrive | 覆盖常见外设、显示器和可移动存储，保留内置 USB 设备 |
| find | class 下推；name/id 在 Node 中字面匹配 | HardwareID 是数组，不能先用 DeviceID 筛掉仅硬件 ID 命中的设备 |

core 网卡仅取 NetConnectionID 非空的条目，保留虚拟网卡。PhysicalAdapter 只展示，不用来排除：历史样机上的 VMware/Wintun 也曾返回 true。

## 输入与数据可信度

- find 至少传一个条件，条件之间 AND；name/id 不区分大小写，不解释通配符。
- class 限定为允许的类名字符；动态字符串通过公共 psString 作为数据传入 PowerShell。
- 设备名和错误码原样返回。ConfigManagerErrorCode 不做本地化翻译，驱动是否过旧也不在代码中硬编码。
- 采集失败保留 collectionErrors/degraded。无蓝牙或外设是合法情况，但只有查询成功时，空清单才可按“未枚举到”解释。
- pwsh 随包分发；WMI 和相关类仍依赖目标系统。无需按旧于 Win10 的版本维护分支。

## 已知边界

| 边界 | 影响 |
|---|---|
| 飞行模式、射频开关未读取 | 设备正常不等于无线功能可用 |
| 网络打印机不一定走 PnP | external 不能当作全部打印机清单 |
| 内置与外置可能使用同一总线前缀 | 需结合设备名和机型判断 |
| PNPClass、HardwareID 等字段可能为空 | 保留缺失，不推断无设备 |
| 不枚举完整驱动仓库 | 需要时另行设计 store 能力，当前未实现 |

驱动更新、卸载、回滚属于后续处置建议，不纳入只读工具。版本应与厂商资料另行核对。

## 实施记录

2026-09-03 四个 scope 完成，本地 `_t12.mjs` 的 25 项验证通过；后续硬件 ID 和脚本字符串问题在 2026-09-05 修复。历史样机数据见 `test/testlog/`，当前接口以 [工具文档](../../tool/driver.md) 为准。
