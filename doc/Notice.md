# 运行注意事项

## 运行边界

默认工具不修改系统配置，系统仍按当前进程权限允许操作。未知扩展与手动命令需单独核查其读写行为。
配置与会话保存在 `agent/home`，临时状态保存在 `.state`。Windows 日志和 PowerShell 原生缓存不保证完全留在 U 盘。

本项目与常规 pi Agent 用法不同的地方。完整论证分散在各开发文档中，
这里只留一份清单：每条给出结论和权威出处，未指向出处的条目以本文为准。

## AGENTS.md 不会生效

`--no-context-files` 关闭的不只是项目上下文文件，也包括 `agent\home` 下的全局
AGENTS.md（加载逻辑在 `noContextFiles` 时直接返回空列表）。因此 Agent 指令不放在
AGENTS.md，而是放在 `agent\home\APPEND_SYSTEM.md`——pi 内置的追加系统提示词文件，
从 `PI_CODING_AGENT_DIR` 自动发现并附加到默认系统提示词之后，不受该开关影响，
也无需改动 `pi.cmd` 的启动参数。

## 覆盖内置工具须提供提示词片段

自定义 `ls` 覆盖内置工具后，若不自带 `promptSnippet`，`ls` 会从
`Available tools:` 列表消失；`label` 只用于 TUI 显示，不进提示词。
详见 [doc\tool\README.md](tool/README.md)「提示词」。

## 模型上下文与 TUI 渲染使用不同的数据

模型读取 `content` 中的文本；`details` 用于界面渲染，不进入模型上下文，
模型消费的信息必须出现在 content 的结果字段与降级说明里；整次失败由 pi 捕获抛错并标记 isError。
详见 [doc\tool\README.md](tool/README.md)「返回结构」。

## pi.cmd 保持纯 ASCII

cmd 按 ANSI/GBK 解析批处理文件，中文内容在部分机器上会导致解析错误。
启动器的说明性内容放在 ASCII 注释中（见 `pi.cmd` 文件头）。

## pwsh 输出解码以 GBK 兜底

`runPwsh()` 对 stdout/stderr 先按 UTF-8 严格解码，失败回退 GBK，
否则中文系统的错误输出（ANSI 代码页）会变成乱码。
详见 [doc\tool\README.md](tool/README.md)「pwsh 调用模式」。

## 零安装下不保证 CPU 核心温度可读

CPU 核心温度读取依赖内核驱动，本项目不安装驱动。目标机已有驱动且权限足够时可能返回读数；降频只作为继续排查的线索。
完整论证与实测记录见 [doc\tool\sys.md](tool/sys.md)「能力边界」。

## 项目信任永久关闭

本工具可能在机主电脑的任意目录启动，cwd 祖先链上可能出现陌生项目的 `.pi\` 资源。
`defaultProjectTrust=never` 保证这些资源不会被加载，是隔离设计的必需项。

## WizTree 扫描方法按卷类型标注

WizTree 在 NTFS 上可直读 MFT，其他文件系统采用目录遍历。`disk usage` 的 `method` 必须反映实际卷类型，探测失败时说明限制，见 [disk](tool/disk.md#扫描与降级)。

1. 文件系统探测先用 fsutil，失败时改用普通权限可访问的 CIM 查询。
2. `runPwsh()` 读取 JSON，查询字符串也须经 `ConvertTo-Json` 输出。
3. CIM 查询超时为 60 秒；结果按卷身份缓存，换盘或无法确认身份时清除。

## 每次启动清除 WizTree3.ini

`WizTree3.ini` 保存 DPI、窗口位置、字号和行列宽。`pi.cmd` 启动时尝试将其重命名为固定的 `WizTree3.ini.bad`，让 WizTree 重建适合当前机器的显示状态；备份保留可能存在的捐赠授权码。

该文件不参与 CLI 结果。WizTree 4.32 的局部验证中，多种异常内容均未阻止导出，文件被独占锁定时出现阻塞；重命名无法替代锁定故障的处理。

## 本项目遥测独立于 pi 自带遥测

`pi.cmd` 的 `PI_OFFLINE=1` 只关 pi 自身的启动联网（版本检查、包更新、模型目录、pi 自带遥测），不拦扩展请求。本项目遥测计划由 `agent\home\extensions\telemetry\` 扩展提供，目前仅有币种映射文件；采集与上报尚未实现。计划中的开关与端点位于 `agent\home\telemetry.json`，与 pi 自带遥测互不影响。pi 自带遥测已由 `PI_OFFLINE=1` 与 `settings.json` 的 `enableInstallTelemetry=false` 关闭。
