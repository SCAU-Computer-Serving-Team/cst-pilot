# 开放工具规格

`cst-kit` 锁定会话开放的 14 个白名单工具，覆盖只读文件与存储分析、系统软硬件诊断、联网检索、修复方案交付与受控 BitLocker 操作。内部架构见 [ARCHITECTURE.md](ARCHITECTURE.md)，参数与返回字段见 [API.md](API.md)，三维度测试矩阵见 [tools-test-spec.md](test/tools-test-spec.md)。

## 开放工具白名单与通用规则

会话创建时固定激活以下 14 个工具，禁用通用命令执行（`bash` / `powershell`）与通用文件写入（`write` / `edit`）工具：

| 类别 | 工具名 | 子功能（`scope` / `action`） | 职责边界 |
|---|---|---|---|
| 文件与存储 | `read` | 无 `scope` | 受路径守卫保护的只读文本与图片读取，统一 JSON 结构与图片双块输出 |
| 文件与存储 | `ls` | 无 `scope` | 覆盖内置 `ls`，按大小倒序展示目录直接子项，支持逐层下钻 |
| 文件与存储 | `disk` | `space`、`info`、`health`、`usage`、`all` | 卷空间、物理盘与逻辑卷、SMART 可靠性、目录与大文件占用排行 |
| 文件与存储 | `fffind`、`ffgrep` | 无 `scope` | 路径模糊相关性查找与多编码内容正则检索 |
| 系统诊断 | `sys` | `overview`、`proc`、`gpu`、`io`、`sensor` | 整机概况、进程 CPU／内存排行、显卡负载与显存、磁盘与进程读写、可读传感器 |
| 系统诊断 | `startup` | 无 `scope` | 注册表自启、启动文件夹与自动启动服务盘点，含禁用三态 |
| 系统诊断 | `eventlog` | `recent`、`boot`、`crash`、`service`、`disk`、`security`、`query`、`detail` | Windows 事件日志场景化查询、按提供程序与事件 ID 聚合计数及单条详情读取 |
| 系统诊断 | `driver` | `problem`、`core`、`external`、`find` | 异常设备、网络／蓝牙／音频／显示核心硬件与签名驱动版本、外设与条件查找 |
| 受控卷执行 | `bitlocker` | `status`、`unlock`、`decrypt` | 实时查询卷加密状态、48 位恢复密码解锁、经本地确认发起后台完整解密 |
| 联网检索 | `web_search`、`web_fetch`、`get_search_content` | 无 `scope` | 支持 Exa（默认，含免费回退与付费深度搜索）与 DeepSeek Search API 的网页检索、正文抓取与会话缓存续读 |
| 方案与交付 | `runbook` | `plan`、`export` | 单工具统一管理修复命令方案、版本修订与 `.txt` 清单导出，双端卡片支持一键本地导出并打开 |

通用行为约束：

1. **只读边界与写入例外**：12 个查询与检索工具不修改系统配置、服务状态或机主文件，存储诊断零临时文件落盘。`runbook` 保存方案至 `data/plans/`，导出清单至 `artifacts/<日期>/runbook/`。`bitlocker` 仅 `unlock` 与 `decrypt` 经本地确认后变更卷状态。
2. **敏感路径与内网阻断**：`read`、`fffind` 与 `ffgrep` 统一阻断 `data/config/{auth,models,mcp-auth,web-search}.json`、`data/telemetry/identity.json` 与 `.state/` 目录，同时拦截目录外硬链接、Windows 设备路径（`\\.\`）与 NTFS 备用数据流。`ls` 与 `disk(usage)` 在明细、排行与拒绝列表中隐去敏感路径名，体积计入总容量。联网工具每次请求、重定向及云端抓取前先校验解析后的 IP，拒绝回环与私网地址，按配置放行 `198.18.0.0/15` TUN 网段。
3. **统一耗时与裁剪保护**：`kit.rs` 外层为全部 14 个工具注入顶层整数毫秒 `elapsedMs`，覆盖正常、降级、结构化错误 JSON 与 `50 KiB` 裁剪输出。模型文本超过 `50 KiB` 时优先裁剪大数组与长正文，保护 `elapsedMs`、`degraded`、`degradedFrom`、`error`、`notice`、`file`、`dir`、`name`、`runbookId`、`version`、`sequence`、`action`、`level`、`levelLabel`、`items` 等核心字段。
4. **口径与错误诚实**：工具说明（`notice`）仅陈述数据来源、单位、采样窗口与采集限制，不输出故障定性或修复建议。请求数据源全部失败时返回结构化错误 JSON（`{"error", "elapsedMs"}`）并置 `isError: true`。部分数据源失败时保留成功数据，置 `isError: false` 与 `degraded: true`。正常调用不输出空错误容器（`collectionErrors`、`counterErrors`）或 `degraded: false`。

## 文件与存储工具

### read

- **文本读取**：按偏移量与行数分页读取，单次上限 2000 行与 `50 KiB`（扫描阶段预留 JSON 转义余量），返回单块 JSON，含行号窗口、`truncated`、`truncatedBy`、`nextOffset` 与 `content`。不输出失效的外部命令提示。
- **图片读取**：经 `PathGuard` 单句柄核验后执行 EXIF 修正与自适应缩放，返回 `[Text(meta_json), Image(base64)]` 双块。`details` 仅保存轻量元信息，不重复保存图片 base64。

### ls 与 disk

- **ls 大小来源与预算**：实时枚举目标目录直接子项，不设条目数上限，统一受外层 215 秒超时与取消信号控制。管理员权限下查询本地 NTFS 盘根且无缓存时建立 120 秒 `$MFT` 卷缓存，同卷子目录查询优先复用快照。全部直接子目录通过 `FileReference` 与 `PathPolicy::LIVE` 路径核验时返回 `method: "native-index"`，任一项未命中则整次走并行 Win32 目录遍历（`method: "walk"`）。`$MFT` 扫描失败回退遍历时附加 `degradedFrom` 与 `degraded: true`。目录不可打开输出 `bytes: null`，任一子项不完整时全部 `pct` 置 `null` 并置 `degraded: true`。
- **disk space / info / health**：容量按二进制 GiB（`字节 ÷ 2³⁰`）换算，`freeGB: 0` 表示剩余空间为零。`space` 与 `volumes` 走 Win32 原生接口并带线程防弹窗与远程卷短超时隔离。盘符映射优先 `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS`（全 extent 核验，失败回退 WMI）。`health` 优先 Win32 原生 NVMe/ATA 存储 IOCTL（核验序列号并固定输出 6 项字段），不支持或身份不明时回退单次批量 WMI。指定 `drive` 且关联失败时，`info` 返回全量 `physicalDisks` 并附 `infoNotice`，`health` 拒绝查询 SMART 并说明原因。
- **disk usage 扫描与降级**：管理员本地 NTFS 盘根默认使用纯 Rust `ntfs-ng` 直读 `$MFT`（`method: "native-mft"`，设 `CST_DISABLE_MFT=1` 关闭），完整快照写入 120 秒卷缓存供 `ls` 复用，并补齐 `stats.filesScanned`。记录损坏、超内存上限或卷身份变化时拒绝发布缓存，回退 `native-walk` 并附 `degradedFrom` 与 `degraded: true`。子目录、普通权限、非 NTFS 卷与 UNC 路径走无条目上限并行 Win32 `FileIdBothDirectoryInfo` 遍历。两套后端均执行确定性硬链接去重与云占位保护。

### fffind 与 ffgrep

- **单次基线与并发边界**：每次检索建立瞬态 `PathGuardSession`，不跨调用缓存。遍历禁用符号链接与目录联接跟随（`follow_links(false)`），工作线程上限 8 线程（预留 2 个逻辑核余量），打开文件后在同一句柄上复核规范路径、硬链接数与文件标识。
- **路径排序与跨盘过滤**：`fffind` 按相关性排序（精确文件名与前缀高于分散子序列），支持空格多词交集，全为弱匹配时附 `notice`。`include` 与 `exclude` 统一按正斜杠路径匹配，跨盘符绝对路径与相对路径行为一致。
- **多编码解码与资源护栏**：`ffgrep` 依次处理 UTF-16 LE/BE BOM、UTF-8 BOM、无 BOM 含 `0x00` 二进制跳过、UTF-8 零拷贝与 GB18030 回退解码。单文件上限 `10 MiB`（超限跳过并记入 `skippedLargeFiles` 与 `notice`），单行上限 `8 KiB`，单次总读取上限 `256 MiB`（达到上限保留已有命中并置 `truncated: true` 与 `degraded: true`），最多遍历 50 万项，超时 30 秒，输出超 `50 KiB` 前受控截断并置 `truncated: true`。

## 系统诊断工具

### sys 与 startup

- **sys overview**：优先通过 Win32 原生接口采集内存（`GlobalMemoryStatusEx`）、内核池（`GetPerformanceInfo`）、开机时长（`GetTickCount64`）、页面文件（`K32EnumPageFilesW`）、核心数（`GetLogicalProcessorInformationEx`）与注册表机型／BIOS，CPU 总占用采用 `GetSystemTimes` 200 ms 短窗双采样。原生接口缺失单项时回退 WMI。
- **sys proc / io 双采样**：`proc` 与 `io` 共用 `NtQuerySystemInformation(SystemProcessInformation)` 单次全量进程快照，覆盖全部系统与受保护进程，按 `(pid, create_time)` 差分防 PID 复用，仅对 `proc` 最终 Top-N 进程复用单缓冲查询路径。`io` 物理盘指标走 PDH `PhysicalDisk`，与进程双采样共用约 `1.2 s` 窗口。原生接口报错时回退 WMI 并标记 `degraded: true`。
- **sys gpu 聚合与双显卡核显补充**：每进程 `gpuPct` 取跨适配器最繁忙单一引擎利用率，`engines[]` 仅保留 `gpuPct > 0` 样本。专用显存仅统计大于 1 MiB 的实例。进程名映射使用 `CreateToolhelp32Snapshot`。检测到非 NVIDIA 物理显示适配器时，在 PDH `1.2 s` 窗口内并行执行 `lhm(Gpu)` 与 `nvidia-smi`，双显卡机型同时返回独显与核显数据。LHM 传感器统一附 `unit` 字段，采集失败写入 `lhmGpu.error`。PDH 本地化计数器路径在进程内缓存复用。
- **sys sensor 能力边界**：零安装约束下不安装 WinRing0 或 PawnIO 内核驱动，并行读取随包 LHM 的温度、风扇、电压传感器（附 `unit`，上限 200 项）与 PDH ACPI 热区（−50～150°C）、逻辑核频率百分比（保留 >100% 睿频值）。
- **startup 禁用三态与 SCM**：采集 HKLM/HKCU 的 `Run`、`RunOnce`、`Wow6432Node\Run`、启动文件夹与自动启动服务。自启服务优先经 Win32 SCM（`EnumServicesStatusExW` 与 `QueryServiceConfigW`）枚举，失败回退 WMI。关联 `StartupApproved` 首字节奇偶判读 `disabled`（`true` / `false` / `null`），不将 `Run` 的禁用记录套给同名 `RunOnce`。超 140 字符的 `path` 截断并附 `…`。

### eventlog 与 driver

- **eventlog 场景、渲染与时区**：采用 `EvtNext(64)` 与 `EvtRender(EvtRenderEventValues)` 复用缓冲扫描，`query` 中的字面量 `provider` 条件直接下推 XPath。`events[].time` 输出本机带偏移 RFC3339（如 `2026-11-26T00:35:00+08:00`），与 `sys(overview).uptime.bootTime` 时区对齐。`detail` 正文超过 `20,000` 字符时附 `msgTruncated: true` 与 `originalChars`。`security` 执行前检查管理员令牌，非管理员返回 `admin: false, degraded: true`。请求通道全部失败返回 `isError: true`，部分通道失败保留已读通道并标 `degraded: true`。
- **driver 枚举与查找**：`problem` 筛选 `Status=Error/Unknown`。`core` 的关键服务（`bthserv`、`Audiosrv`）优先经 SCM 查询，四类签名驱动（`NET`、`BLUETOOTH`、`MEDIA`、`DISPLAY`）优先经 SetupAPI 查询，失败回退 WMI，`drivers[].date` 统一格式化为 `YYYY-MM-DD`。`external` 覆盖 `USB*`、`BTHENUM*`、`DISPLAY*` 与可移动磁盘。`find` 要求至少一个条件并按 AND 组合，`id` 同时匹配 `deviceId` 与 `hardwareIds`。

## 联网检索工具

1. **三工具分工**：`web_search` 执行 1–3 个关键词并发检索；`web_fetch` 执行 1–5 个网页链接并发正文抓取；`get_search_content` 对两者的会话内存缓存（上限 32 条 / 8 MiB / 单条 2 MiB / TTL 30 分钟）执行分页或关键词定位续读。
2. **双提供商与默认可用性**：配置文件 `data/config/web-search.json`（v2，兼容读取 v1）默认启用 `exa` 且开启托管免费 MCP 回退，保证便携 U 盘免配即用。配置 Exa 付费 Key 后使用官方 REST `/search`（支持 `fast` / `auto` / `deep` 模式，`lite` 自动剥离不支持参数）与 `/contents`。可选切换为 `deepseek` 独立 JSON REST 接口（`/v1/tools/search` 128 字符查询守卫与 `/v1/tools/fetch`），支持独立 Key 或 `reuseModelApiKey` 复用模型凭据。
3. **并发、降级与编码安全**：多词与多链接受控并发执行并保持输入顺序，单条失败记入对应条目 `error` 与顶层 `collectionErrors` 并标 `degraded: true`，全部失败返回 `isError: true`。抓取策略支持 `auto` / `provider` / `direct`，所有 URL 发起前先经本地 SSRF 策略校验，直连抓取支持 UTF-8 与 GB18030 解码。

## 命令方案与交付

1. **单工具统一流程**：`runbook` 通过 `action: "plan"`（默认）创建或修订命令方案，返回稳定 `runbookId`、当天序号 `sequence`（`01–999`）与递增 `version`。修订必须同时传入 `runbookId` 与当前最新 `baseVersion`，冲突时拒绝覆盖并保持原序号不变。
2. **导出与双端一键直出**：传入 `exportTxt: true` 或调用 `action: "export"`（传 `runbookId` 与 `version`）时导出至 `artifacts/<YYYY-MM-DD>/runbook/<序号>-<风险档>-<标题>-v<版本>.txt`。桌面端与 Web 端方案卡片支持点击按钮直接调用服务端 `POST /api/sessions/:id/runbook/export` 导出 `.txt` 并用系统记事本打开，不触发额外模型推理。
3. **格式统一与防篡改**：`commands[].shell` 统一输出小写 `"cmd" | "powershell"`。导出文件固定采用 UTF-8 BOM + CRLF 编码，每条命令四行正文加空行分隔，命令含换行或必填项为空时整次失败。同版本文件内容未变时幂等复用，已导出文件被现场修改或尝试导出过期版本时报错拒绝覆盖。各级目录拒绝符号链接与重解析点，目标文件拒绝硬链接，经同目录临时文件同步后原子替换。

## bitlocker：状态、解锁与解密

| 操作 | 行为与安全规则 |
|---|---|
| `status` | 实时查询指定盘或全部卷的加密与保护状态，不使用缓存快照 |
| `unlock` | 仅支持 48 位数字恢复密码。已解锁卷直接跳过，提交前复核卷身份与锁定状态，调用后再次查询确认解锁完成 |
| `decrypt` | 执行前固定目标卷清单并在本地界面确认解除保护影响。锁定卷或系统卷残留自动解锁依赖时原样报错，不自动解锁或删除保护器。发起后台转换后立即返回，仅在完全解密时确认完成 |

凭据与恢复边界：

1. **双路径输入与隐私保护**：默认通过本地隐藏输入框（180 秒超时）采集恢复密码，内存使用后立即清零，全程不进入模型上下文、会话记录、事件流或遥测。AI 填参路径须经本地确认传输边界，在输入预检、消息落盘与事件广播前统一脱敏，同时保留发给模型的工具参数定义。
2. **同卷互斥与中断实时复查**：同一加密卷跨进程互斥拒绝并发操作。执行期间遇到超时、系统调用异常或进程强杀重启时，禁止自动重放命令，标记操作结果未知并立即重新查询卷真实状态。
