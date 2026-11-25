# 诊断与受控工具规格

`diagnostics` 模块锁定会话开放的 17 个白名单工具，提供只读软硬件诊断与安全检索能力，同时支持手动修复命令交付与受控 BitLocker 执行。内部架构见 [ARCHITECTURE.md](ARCHITECTURE.md)，调用参数与返回字段见 [API.md](API.md)。

## 开放工具白名单与通用规则

会话创建时固定激活以下 17 个工具，移除通用命令执行（`bash` / `powershell`）与文件写入（`write` / `edit`）工具：

| 类别 | 工具名 | 子功能（`scope`） | 职责边界 |
|---|---|---|
| 存储与系统查询 | `ls` | 无 `scope` | 覆盖内置 `ls`，按大小倒序展示目录直接子项，支持逐层下钻 |
| 存储与系统查询 | `disk` | `space`、`info`、`health`、`usage`、`all` | 卷空间、物理盘与逻辑卷、SMART 可靠性、目录与大文件占用排行 |
| 存储与系统查询 | `sys` | `overview`、`proc`、`gpu`、`io`、`sensor` | 整机概况、进程 CPU／内存排行、显卡负载与显存、磁盘与进程读写、可读传感器 |
| 存储与系统查询 | `startup` | 无 `scope` | 注册表自启、启动文件夹与自动启动服务盘点，含禁用三态 |
| 存储与系统查询 | `eventlog` | `recent`、`boot`、`crash`、`service`、`disk`、`security`、`query`、`detail` | Windows 事件日志场景化查询、按提供程序与事件 ID 聚合计数及单条详情读取 |
| 存储与系统查询 | `driver` | `problem`、`core`、`external`、`find` | 异常设备、网络／蓝牙／音频／显示核心硬件与驱动版本、外设与条件查找 |
| 文件与联网检索 | `read`、`fffind`、`ffgrep` | 无 `scope` | 受路径守卫保护的只读文件读取、路径模糊查找与内容正则检索 |
| 文件与联网检索 | `web_search`、`source_check`、`fetch_content`、`get_search_content` | 无 `scope` | 受 SSRF 防护的 Exa 网页检索、来源核验、正文抓取与缓存续读 |
| 命令方案与交付 | `command_plan`、`runbook_deliver`、`runbook` | 无 `scope` | 多版本修复方案保存、核对后导出 TXT 清单及单步纯文本清单生成 |
| 受控卷执行 | `bitlocker` | `status`、`unlock`、`decrypt` | 实时查询卷加密状态、48 位恢复密码解锁、经本地确认发起后台完整解密 |

通用行为约束：

1. **只读与写入例外**：查询与检索工具不修改系统配置、服务状态或机主文件，存储诊断全程零临时文件落盘。`command_plan` 写入 `data/plans/`，`runbook_deliver` 与 `runbook` 写入 `artifacts/<日期>/runbook/`。仅 `bitlocker` 的 `unlock` 与 `decrypt` 经本地确认后变更卷状态。
2. **敏感路径与内网阻断**：`read`、`fffind` 与 `ffgrep` 统一阻断 `data/config/{auth,models,mcp-auth,web-search}.json`、`data/telemetry/identity.json` 与 `.state/` 目录，同时拦截目录外硬链接、Windows 设备路径（`\\.\`）与 NTFS 备用数据流。`ls` 与 `disk(usage)`（含原生遍历与 `$MFT` 后端）在明细、排行与拒绝列表中隐去上述敏感路径名，其体积仍计入总容量。联网工具每次请求与重定向前校验解析后的 IP，拒绝回环与私网地址，仅按配置放行 `198.18.0.0/15` TUN 假 IP 网段。
3. **口径与错误诚实**：工具固定说明（`notice`）仅陈述数据来源、单位、采样窗口与采集限制，不输出故障定性或修复建议。部分数据源失败时保留成功数据并标记 `degraded: true`。整次查询失败时返回错误并标记 `isError: true`。空数组或 `null` 必须结合错误字段判读，禁止直接当作健康或零值。

## 查询工具规则

### ls 与 disk（存储分析）

- **ls 大小来源与预算**：每次在单一调用级预算（**50 万条目、30 秒**）内实时枚举目标目录直接子项。默认使用受控并发 Win32 目录遍历一次统计所有直接子目录，直接文件与兄弟子目录共享查询范围确定性硬链接去重表（跳过符号链接、重解析点与云占位目录；完全驻留云文件正常计入）。目录自身不可打开时输出 `bytes: null`。实验开关 `CST_ENABLE_MFT=1` 开启且全部直接子目录均命中同卷有效 `$MFT` 快照（核验 `FileReference` 与 `PathPolicy::LIVE` 路径一致）时返回 `method: "native-index"`，否则整次统一走 `walk`，不混用两套归属口径。超限或遇到权限拒绝时停止并标注 `totalSize` 为下界，任一子项不完整时全部 `pct` 置 `null`。UNC 路径直接走原生遍历。
- **disk space / info / health**：容量按二进制 GiB（`字节 ÷ 2³⁰`）换算，`freeGB: 0` 表示剩余空间为零。`space` 与 `volumes` 使用 Win32 原生接口（带线程防弹窗与网络卷统一短超时隔离，`all` 复用同次卷探测），盘符映射优先 `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS`（要求全部 extent 可验证，否则回退 WMI），`health` 优先 Win32 原生 NVMe/ATA 存储 IOCTL（交叉核验序列号并严格输出 `API.md` 6 项字段），不支持或身份不明时回退单次批量 WMI。指定 `drive` 且关联失败时，`info` 返回全量 `physicalDisks` 并附 `infoNotice`，`health` 拒绝查询 SMART 并说明原因。SMART 仅在遭遇权限拒绝时提示管理员权限，`smart: null` 不代表硬盘健康或损坏。
- **disk usage 扫描与降级**：发行路径默认使用受控并发 Win32 `FileIdBothDirectoryInfo` 遍历（`method: "native-walk"`）。仅在通过实盘门禁或显式设置实验开关 `CST_ENABLE_MFT=1`（且未设 `CST_DISABLE_MFT=1`）时，已提权的本地 NTFS 盘根尝试纯 Rust `ntfs-ng` 直读 `$MFT`（`method: "native-mft"`），损坏或缺失记录拒绝发布快照并自动回退 `native-walk` 附 `degradedFrom`。两套后端均输出 `topDirs`（含不同深度，父子不可相加）、`topFiles`（`{ path, sizeGB, modified }`）、`extAgg`、`staleFiles`（≥50 MiB 且 ≥1 年未修改），并执行确定性硬链接去重与云占位保护。

### sys 与 startup（实时负载与启动配置）

- **sys proc / io 双采样**：`proc` 在进程内间隔约 1.2 秒双采样，按实际间隔与逻辑核数计算 `cpuPct`，内存取工作集 `wsMB`。`io` 在自身约 1.2 秒双采样窗口内同步读取物理盘忙碌度、队列长度（排除 `_Total`）与进程读写字节差分，两套指标口径不同不可直接相加。
- **sys gpu 聚合与补充**：每进程 `gpuPct` 取跨适配器最繁忙单一引擎利用率，不累加并行引擎。专用显存仅统计大于 1 MiB 的实例。并行查询 `nvidia-smi`（带 `uuid` / `pciBusId`），缺失或失败时由随包 LHM 补充。计数器路径按系统语言自动本地化，引擎查询失败自动重试一次。
- **sys sensor 能力边界**：零安装约束下不安装 WinRing0 或 PawnIO 内核驱动，仅读取 LHM 的 CPU、GPU、主板用户态传感器（过滤非有限值，上限 200 项）、ACPI 热区（仅保留 −50～150°C）与逻辑核频率百分比。低频率仅作排查线索，不能单独确认过热。
- **startup 禁用三态**：采集 HKLM/HKCU 的 `Run`、`RunOnce`、`Wow6432Node\Run`，以及个人与公共启动文件夹和自启服务。关联 `StartupApproved` 首字节奇偶判读 `disabled`（`true` 已禁用 / `false` 已启用 / `null` 无匹配记录），禁止将 `Run` 的禁用记录套给同名 `RunOnce` 项。PE 环境如实返回 PE 自身注册表与服务。

### eventlog 与 driver（历史日志与设备状态）

- **eventlog 场景与权限**：内置场景事件范围见 [API.md](API.md#eventlog-内置事件范围)。`boot` 合并系统启停与 WHEA 并按 `logName + recordId` 去重后再截断 `top`。`crash` 保留 Information 级 WER（1001）与 Hang（1002），`app` 子串过滤作用于全部分组，`7045` 归入 `boot` 新装服务。`security` 执行前检查管理员令牌，非管理员直接返回 `admin: false, degraded: true`。
- **driver 枚举与查找**：`problem` 筛选 `Status=Error/Unknown`。`core` 网卡仅保留 `NetConnectionID` 非空项，签名驱动版本按 `Net`、`Bluetooth`、`MEDIA`、`Display` 四类查询。`external` 覆盖 `USB*`、`BTHENUM*`、`DISPLAY*` 与可移动磁盘。`find` 要求至少传入一个条件，多个条件按 AND 组合，`id` 同时匹配 `deviceId` 与 `hardwareIds`。

## 命令方案与交付

1. **多轮方案**：每批新增或修改修复命令均调用 `command_plan`。新建返回稳定 UUID 与版本 1。修订须同时传入 `runbookId` 与当前最新 `baseVersion`，冲突时拒绝覆盖。
2. **文件交付**：队员核对后调用 `runbook_deliver`（仅传 `runbookId` 与 `version`），生成 `artifacts/<日期>/runbook/<ID>/v<版本>-<风险档>-<标题>.txt`。同版本内容未变时直接复用。过期版本或磁盘文本已被现场修改时报错拒绝覆盖。
3. **单步清单**：`runbook` 写入 `artifacts/<YYYY-MM-DD>/runbook/<序号>-<风险档中文>-<标题>.txt`，序号 `01`–`999` 当天唯一递增、跨天重置。传入已有 `sequence` 时原地替换并清理旧标题文件。
4. **编码与链接保护**：所有清单采用 UTF-8 BOM + CRLF 编码，首行写入标题与风险信息并空一行，随后每条命令按固定四行正文加空行分隔排版，命令含换行或必填项为空时整次失败。各级目录拒绝符号链接与重解析点，目标文件拒绝硬链接，经同目录临时文件同步后原子替换。

## bitlocker：状态、解锁与解密

| 操作 | 行为与安全规则 |
|---|---|
| `status` | 实时查询指定盘或全部卷的加密与保护状态，不使用缓存快照 |
| `unlock` | 仅支持 48 位数字恢复密码。已解锁卷直接跳过，提交前复核卷身份与锁定状态，调用后再次查询确认解锁完成 |
| `decrypt` | 执行前固定目标卷清单并在本地界面确认解除保护影响。锁定卷或系统卷残留自动解锁依赖时原样报错，不自动解锁或删除保护器。发起后台转换后立即返回，仅在完全解密时确认完成 |

凭据与恢复边界：

1. **双路径输入与隐私保护**：默认通过本地隐藏输入框（180 秒超时）采集恢复密码，内存使用后立即清零，全程不进入模型上下文、会话记录、事件流或遥测。AI 填参路径须经本地确认传输边界，在输入预检、消息落盘与事件广播前统一脱敏，同时保留发给模型的工具参数定义。
2. **同卷互斥与中断实时复查**：同一加密卷跨进程互斥拒绝并发操作。执行期间遇到超时、系统调用异常或进程强杀重启时，禁止自动重放命令，标记操作结果未知并立即重新查询卷真实状态。
