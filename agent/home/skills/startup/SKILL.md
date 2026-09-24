---
name: startup
description: startup 工具的返回字段说明。覆盖三块数据的内容、disabled 三态的含义、数据来源与已知边界。
---

# startup 工具说明

盘点开机自启项的只读工具。无参数，一次获取三类配置。耗时受 WMI 与便携 PowerShell 启动速度影响。

## 数据来源

1. 注册表自启项：`Get-Item` 枚举 HKLM 与 HKCU 的 Run / RunOnce 键，另含 HKLM Wow6432Node\Run（32 位程序在 64 位系统上的自启落点）
2. 禁用状态：读 `StartupApproved` 键（任务管理器"启动应用"开关的落点，该开关不删除 Run 键，只写此二进制值），首字节奇数表示已禁用
3. 启动文件夹：当前用户与所有用户的 Startup 文件夹内文件列表
4. 自启服务：`Win32_Service` 中 StartMode=Auto 的记录（含延迟自启）

## 返回字段定义

1. `regItems`：注册表自启项。`source` 为所在注册表键；`name` 为项名；`command` 为实际执行的命令行；`disabled` 为禁用状态
2. `startupFolders`：`scope` 为 user（当前用户）或 allUsers（所有用户）；`path` 为文件夹路径；`items` 内每项含 `name` 与 `disabled`
3. `services`：`name` / `display` / `state`（Running 的排在前面）/ `path`（超过 140 字符截断）
4. `disabled` 三态：`true` 表示匹配的 StartupApproved 记录为禁用；`false` 表示启用；`null` 表示没有可用的匹配状态。注册表存在条目或状态为启用，都不能保证实际启动成功

## 已知边界

1. 不含计划任务、WMI 事件订阅等非 Run 键持久化机制
2. StartupApproved 按注册表根、Run / Run32 类别与名称匹配；启动文件夹按注册表根与文件名匹配。RunOnce 不套用 Run 状态
3. 服务列表保留系统与第三方服务，不按厂商过滤；System32 路径不能单独证明归属或可信度
4. 结果中的 `notice`、`degraded` 与 `collectionErrors` 需结合解读。采集失败时，空清单不能解释为没有自启项
