# 运行注意事项

本项目与常规 pi Agent 用法不同的地方。完整论证分散在各开发文档中，这里只留一份清单：每条给出结论和权威出处，未指向出处的条目以本文为准。

## 运行边界

默认工具不修改系统配置，系统仍按当前进程权限允许操作。未知扩展与手动命令需单独核查其读写行为。

配置与会话保存在 `agent/home`，临时状态保存在 `.state`。Windows 日志和 PowerShell 原生缓存不保证完全留在 U 盘。

## 管理员启动

`pi.cmd` 通过随包 `launcher.ps1` 检查当前权限。未提权时使用 Windows UAC 请求管理员授权，授权后启动一份 Pi；取消、拒绝或提权失败时退出，不以普通权限继续。

| 场景 | 行为 |
|---|---|
| 普通权限交互启动 | 请求 UAC，优先新开管理员 Windows Terminal（`wt`）窗口 |
| 管理员 WT 会话 | 留在原窗口，不重复打开 |
| 管理员传统控制台 | 优先转入 WT |
| WT 未安装或启动请求失败 | 使用系统控制台，不安装软件或修改默认终端设置 |
| 取消或拒绝 UAC | 退出，不重复请求授权 |
| RPC、打印或自动冒烟使用管道 | 调用方须已提权；保留原连接，不转入 WT；未提权时直接报错 |

Pi 及其诊断子进程统一使用管理员权限。只读范围由工具实现和扩展白名单限制，启动器不构成系统权限隔离；不关闭 UAC、不保存管理员密码、不创建服务或计划任务。提权后重新建立工具包环境，保留原工作目录和启动参数，不使用管理员账户的个人目录定位项目配置。

## AGENTS.md 不参与发行

项目开发用的 `AGENTS.md` 不进入发行包。装配白名单与发行树校验共同排除所有目录下的该文件。

### 上下文加载

`--no-context-files` 关闭的不只是项目上下文文件，也包括 `agent\home` 下的全局
AGENTS.md（加载逻辑在 `noContextFiles` 时直接返回空列表）。Agent 指令放在
`agent\home\APPEND_SYSTEM.md`——pi 内置的追加系统提示词文件，
从 `PI_CODING_AGENT_DIR` 自动发现并附加到默认系统提示词之后，不受该开关影响，
也无需改动 `pi.cmd` 的启动参数。

## 网页抓取与 TUN 代理

发行包的 `agent/home/web-search.json` 仅放行 `198.18.0.0/15`，供使用 TUN 假 IP 代理时抓取公开网页。配置不含服务密钥或代理地址，`trustEnvProxy` 保持 false；localhost、本机回环、其他私网地址及指向这些地址的重定向仍被拒绝。

若现场代理使用其他地址段，需先确认用途再显式调整 `ssrf.allowRanges`。放行不保证目标站点可访问，HTTP 错误、反爬和模型回答错误仍单独报告。

## 双端共享与独立主题

模型范围、默认模型、凭据与历史由 TUI 和 Web 共用。打开模型菜单、循环模型或发送新输入时重读共享配置；当前会话与全局默认的区别见[模型规格](web/SPEC/auth.md)。

TUI 主题保存在 Pi 的 `settings.json`；Web 主题保存在 `web-settings.json`，默认浅色。两端互不覆盖，TUI Automatic 由 Pi 自行处理。

同一会话只允许一个参与运行协调的实例。另一端持有记录时先退出该端，再打开；手动脚本或未加载运行协调扩展的外部 Pi 不能同时改写同一记录。原会话目录、AGENTS 禁用与写权实现见[运行设计](design/web/runtime.md)。

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
`pi.cmd` 只保留 ASCII 入口；权限请求、便携环境与运行时选择由随包 `launcher.ps1` 处理。

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

`pi.cmd` 的 `PI_OFFLINE=1` 只关 pi 自身的启动联网（版本检查、包更新、模型目录、pi 自带遥测），不拦扩展请求。项目遥测由 `agent\home\extensions\telemetry\` 扩展提供，发行包生成 `agent\home\telemetry.json`，同时向 `https://www.cstoa.top/api/telemetry` 与 `https://8.163.28.9:8445/api/telemetry` 上报，两端使用独立队列。

Tim 端点的专用公开 CA 随扩展发行，只用于该端点的 TLS 验证。它与 pi 自带遥测互不影响；pi 自带遥测已由 `PI_OFFLINE=1` 与 `settings.json` 的 `enableInstallTelemetry=false` 关闭。
