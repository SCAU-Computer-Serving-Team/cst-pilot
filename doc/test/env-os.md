# Windows 版本与裁剪镜像验证

范围为 Windows 10/11 x64。先执行 [公共组 B01–B35](README.md#公共测试组-b01b35)，再检查模块、语言和裁剪差异。记录具体版本号与镜像来源。

## 样本选择

| 系统 | 重点 |
|---|---|
| Windows 11 家庭版/专业版 | 当前常用基线，保留已测记录 |
| Windows 10 1507–22H2 | 优先补较早版本，记录便携运行时和 cmdlet 是否可用 |
| 政府专用或政采裁剪镜像 | 缺失服务/模块、安全软件、UKey/CA Key/国密卡 |
| 不同系统语言 | 计数器路径与事件消息渲染 |

## 差异项

| ID | 操作 | 独立核查 | 通过条件 |
|---|---|---|---|
| O01 | 任一需 pwsh 的工具 | 包内 pwsh 的 PSVersionTable | 实际使用包内运行时；无法启动则明确记录，不依赖系统 PowerShell 代替 |
| O02 | disk info/health | Get-Command、存储模块和 CIM 可用性 | 模块缺失或查询失败有具体错误，不假设存在未实现的备用数据源 |
| O03 | sys overview/io/gpu/sensor | CIM 类与当前语言计数器 | CIM 类查询正常；GPU/热区/频率路径可本地化或明确失败 |
| O04 | eventlog recent | Get-WinEvent 同条记录 | 消息按系统资源渲染，levelName 使用固定英文；无消息文本可为 null |
| O05 | 本地工具采集组 | 进程、服务及返回错误 | 工具采集不要求云服务；pi 的模型请求网络需求另行记录 |
| O06 | driver problem/find，查询安全软件相关设备 | PnP 厂商/名称与软件配置 | 实际设备如实列出，不仅因名称或虚拟属性判异常 |
| O07 | 插入已有 UKey/CA Key 后 external/find | PnP VID/PID 与设备管理器 | 枚举和 find 相互一致；无设备节点时记录原因 |
| O08 | eventlog recent | Get-WinEvent -ListLog 的可用通道 | 正常通道可查；裁剪或不可访问通道明确报错 |

裁剪导致的预期降级可以通过错误语义检查，但仍须标为该能力不可用，不能记成完整功能通过。
