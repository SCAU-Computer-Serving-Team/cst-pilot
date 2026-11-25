# 17 个开放工具设计评估与三维度测试规范

本文面向 `crates/cst-kit` 开放的 17 个工具，梳理现有实现、外部依赖与测试缺口，围绕**兼容性**、**性能**与**字段语义正确性**定义补充测试矩阵。工具接口定义见 [API.md](../API.md)，公共实机核查组（B01–B35）与自动化入口见 [TESTING.md](../../test/TESTING.md)。

## 1. 现状诊断与测试缺口

当前 `crates/cst-kit/tests/` 已有较完整的安全守卫与控制流替身测试（`diagnostics_contract.rs` 17 项、`storage_scan.rs` 21 项、`sys_load.rs` 12 项、`tools_and_security.rs` 12 项、`runbook_*.rs` 20 项、`bitlocker_*.rs` 25 项），但存在三类系统性缺口：

1. **真实 Windows API 覆盖偏低**：`disk`、`sys`、`startup`、`eventlog`、`driver`、`bitlocker` 的自动化测试全部基于 `DiagnosticSource` / `HardwareSource` / `BitLockerSource` 内存替身，仓库内缺少针对真实 WMI、PDH、Event Log、注册表与 `Win32_EncryptableVolume` 的本机集成用例。
2. **极端数据与并发性能无基准**：缺少大目录（10 万级文件）、高频事件通道（10 万条）、多核/多盘/多进程并发抢占全局 4 槽位，以及长耗时路径取消释放的量化门槛测试。
3. **语义契约存在源码漂移**：部分工具在全源失败判定、非 UTF-8 编码、空结果归因、50 KiB 裁剪、以及与前端卡片投影（`tool_presenter.rs`）和 `SKILL.md` 的字段约定上存在偏差。

## 2. 三项测试维度与验收门槛

| 维度 | 核心覆盖面 | 统一验收红线 |
|---|---|---|
| **兼容性** | Windows 10 1809–Windows 11 25H2、家庭中文版、精简镜像、WinPE；管理员与标准用户；中英文系统语言与 GBK/UTF-16 日志；NVMe/SATA/USB/动态盘/存储空间/光驱/断连网络盘；独显/核显/双显卡/无显卡；TUN 与 HTTP 代理、离线模式 | 任一受限或缺失接口禁止崩溃或静默假成功；权限不足、服务停用、命名空间缺失与无匹配必须区分呈现；中文路径、空格、括号与本地化性能计数器路径正常工作 |
| **性能** | 冷/热启动耗时、WMI/PDH/EvtQuery/文件遍历吞吐、内存与读取字节上限、全局 4 槽位并发隔离、取消令牌响应时间、50 KiB 输出上限 | 常规只读诊断 P95 ≤ 5 秒（双采样 `proc`/`io`/`gpu`/`sensor` ≤ 4 秒）；取消后 1 秒内返回给调用方且不泄漏孤儿进程；大目录与大日志在预算边界内稳定收口，单工具慢调用不拖死会话主循环 |
| **字段语义正确性** | 单位（GiB/MiB/KiB/s/°C）与字段名一致；三态语义（`null` / `0` / `[]`）；`degraded`、`noMatch`、`isError` 互斥与组合边界；截断标记（`truncated` / `scanTruncated` / `childrenTruncated` / `outputTruncated`）；脱敏与四方契约（Schema / 实现 / `tool_presenter` / `SKILL.md`） | 缺失值统一为 `null`，禁止用 `0` 或空数组冒充正常；全源失败返回 `isError: true`；50 KiB 裁剪不得破坏合法 JSON 与关键定位字段；凭据与恢复密码零外泄 |

## 3. 17 个工具设计评估总表

| 工具 | 实现与外部依赖 | 现有测试口径 | 已核实的设计缺陷与风险点 |
|---|---|---|---|
| `read` | `pi-runtime` 内置读取 + `PathGuard`（纯 Rust） | `tools_and_security.rs` 覆盖敏感路径、硬链接、ADS 拦截；缺少大文件/编码/图片兼容测试 | `PathGuard::open` 重复两次 `check()`，单次打开触发 15+ 次文件句柄与规范化系统调用；BMP 图片魔数识别为 `image/bmp`，需核对上游视觉模型兼容性；GBK/UTF-16 文本日志按 UTF-8 读取会替换乱码 |
| `ls` | `WzIndex`（`WizTree64.exe`）+ 原生 `walk.rs` | `storage_scan.rs` 覆盖缓存、预算、重解析点；无真实 `WizTree64.exe` 与慢盘实测 | 冷卷首次 `ls` 会触发整卷 WizTree 扫描（最高 180 秒）；`ls.rs:55` 遇到单个不可读目录项直接中止整次调用；0 字节文件在 `ls.rs:83-87` 回退读取旧 `cached_file` 可能返回已截断前的旧大小；`snapshot.is_some()` 时即使子目录未命中缓存仍标 `method: "wiztree-index"`；预算不完整时不标 `degraded: true` |
| `disk` | Win32 `GetDiskFreeSpaceExW` + WMI（`CIMV2`、`ROOT\Microsoft\Windows\Storage`）+ `WizTree64.exe` + `walk.rs` | `diagnostics_contract.rs` 与 `storage_scan.rs` 均为替身测试；无真实 WMI/SMART/WizTree 集成测试 | `win_native/windows.rs:104-115` 每次 WMI 查询重建 COM、连接与 Tokio 运行时（`health` 最多循环 256 次）；`space` 未先过滤无盘光驱与断连映射盘；`physical_ids` 正则 `Disk #(\d+)` 不覆盖动态盘与存储空间；`all` 在关联失败时向 `collectionErrors` 重复写入同一错误两次；`MSFT_PhysicalDisk` 的 `MediaType/BusType/HealthStatus` 为原始数字枚举；`volume.rs:40` 仅找 `WizTree64.exe`（未回退 `WizTree.exe`），且非管理员扫描 NTFS 子目录仍标 `wiztree-mft`；降级方法名仍沿用旧值 `"node-walk"`；字段名 `*GB` 实际按 GiB（$2^{30}$）换算 |
| `sys` | WMI + Win32 Toolhelp + PDH 本地化计数器 + `nvidia-smi.exe` + `lhm/host/cst-lhm-host.exe` | `diagnostics_contract.rs`、`sys_load.rs` 替身测试 + `lhm-host/test.ps1`；无真实 PDH/WMI/显卡集成测试 | 仓库与默认发行装配缺少已编译的 `lhm/host/cst-lhm-host.exe`，导致 `sensor` 与 `gpu` 的 LHM 分支默认报错降级；`pdh.rs:188-192` 将短暂退出的 GPU 进程无效实例记为 `engine` 错误，触发 `sys_load.rs:141-143` 二次 1.2 秒重采并覆写首轮 `memory` 样本；双显卡笔记本存在 `nvidia-smi` 时跳过 `lhmGpu`，核显传感器缺失；`hardware.rs:83` 仅查 `System32\nvidia-smi.exe` 未兜底旧版 `NVSMI` 目录；`io` 用 WMI `Win32_Process` 双采样替代原生进程 I/O API；`overview` 在 `Manufacturer` 为空时丢弃 `bios` 字段；`proc` 的 `cpuPct` 未钳制上界 `100.0` |
| `startup` | Win32 注册表 + 启动目录枚举 + WMI `Win32_Service` | `diagnostics_contract.rs` 仅 1 项替身单测 | `startup.rs` 缺少注册表全失败且服务失败时的全失败错误判定；未采集计划任务与 WMI 订阅（已在 `notice` 声明），但未覆盖中文路径服务、`Wow6432Node` 权限拒绝、损坏 `StartupApproved` 二进制长度（0 字节或短数组）实机测试 |
| `eventlog` | Win32 `EvtQuery` / `EvtRender` / `EvtFormatMessage` | `diagnostics_contract.rs` 5 项替身 + 1 项 XML 解析单测；无真实通道测试 | `events.rs:175-177` 对扫描的每条事件（最高 10 万条）均先调用 `EvtFormatMessage`，即使已被 `provider` 正则过滤或超出 `top`；缺失消息 DLL 导致的 `unreadable` 在过滤前累计，错误触发 `degraded: true`；`eventlog.rs:179-185` 每条命中事件均执行整数组 `sort_by` 且按变长字符串比较 `SystemTime`；`query` 的 `ids` 数组无长度上限，过长会触发 XPath 报错；`security` 在 `source.admin()` 失败（`admin: null`）时仍固定提示需要管理员权限；全通道采集失败时未返回 `isError: true` |
| `driver` | WMI `Win32_PnPEntity` / `Win32_NetworkAdapter` / `Win32_VideoController` / `Win32_Service` / `Win32_PnPSignedDriver` / `Win32_DiskDrive` | `diagnostics_contract.rs` 仅 2 项替身单测 | **`driver.rs:165` 计数错误**：`core` 实际执行 5 次 WMI 查询，代码按 `sources = 6` 判断全失败，导致 WMI 完全瘫痪时不会返回 `Err`；`external` 的 `MediaType LIKE '%Removable%'` 在部分 USB 移动固态硬盘（报告 `External hard disk media`）上会漏检；`find` 每次拉取全量 `Win32_PnPEntity` 再在 Rust 侧过滤，设备多时耗时偏高 |
| `command_plan` | 纯 Rust + 命名管道目录锁 + `data/plans/` JSON | `runbook_and_plan.rs` 19 项 + `runbook_session.rs` 1 项（覆盖较充分） | 返回值经 `diagnostic_result` 走 50 KiB 通用裁剪，接近上限的 20 条长命令方案在模型文本侧可能被截断 `commands[].command`；`prompt_guidelines` 仍引用旧路径 `skills/runbook/SKILL.md` |
| `runbook_deliver` | 纯 Rust + 命名管道目录锁 + `artifacts/<日期>/runbook/` UTF-8 BOM/CRLF | `runbook_and_plan.rs` 覆盖幂等复用、篡改拒绝、链接拦截 | 跨区写入（`data/plans` 与 `artifacts/` 位于不同挂载点或只读介质）需补测试；同 `command_plan` 经 50 KiB 通用裁剪 |
| `runbook` | 纯 Rust + 命名管道目录锁 + `artifacts/<日期>/runbook/` | `runbook_and_plan.rs` 覆盖序号 01–999、覆盖替换、并发锁 | **`shell` 字段大小写不一致**：`runbook` 返回 `"cmd" \| "PowerShell"`，而 `command_plan` / `runbook_deliver` 返回 `"cmd" \| "powershell"`；只读介质与中文长标题截断边界需补实机用例 |
| `bitlocker` | COM/WMI `PacketPrivacy` + `Win32_EncryptableVolume` + `Zeroizing` + 命名管道锁 | `bitlocker_privacy.rs` 7 项 + `bitlocker_execution.rs` 18 项 + 1 项 BSTR 清零单测（控制流覆盖充分） | **`bitlocker/core.rs:157-162` 判定顺序错误**：指定 `drive` 时若因家庭版缺少命名空间或权限不足导致 `volumes` 为空且 `collection_errors` 非空，会先命中“未找到唯一卷”并吞掉真实权限/命名空间原因；缺少真实加密卷与家庭中文版实机回归 |
| `fffind` | 纯 Rust（`ignore` + `globset` + `PathGuard`） | `tools_and_security.rs` 4 项（与 `ffgrep` 共用） | `walker.filter_entry` 对每个遍历条目调用 `PathGuard::check`，每个文件触发约 15–20 次文件系统句柄/规范化调用，万级文件目录极易命中 30 秒超时；绝对路径查询时 `include_path` 保留反斜杠而 `display` 转为正斜杠，导致 `exclude` glob 跨盘符行为不一致；允许空 `pattern` 且无相关性打分，仅按遍历顺序截断 |
| `ffgrep` | 纯 Rust（`ignore` + `regex` + `PathGuard`） | `tools_and_security.rs` 4 项（与 `fffind` 共用） | 在 `filter_entry` 的每次 `PathGuard::check` 之外，`guard.open` 再额外调用两次 `check`，单文件约 50 次系统调用；`line.contains(&0)` 会把 Windows 常见的 UTF-16 LE 文本（如 PowerShell 输出、DISM/CBS 日志、`.reg`）误判为二进制跳过；GBK/GB18030 中文日志经 `from_utf8_lossy` 变成替换符导致中文匹配失败；输出超 50 KiB 直接报错（与诊断工具自动裁剪不同） |
| `web_search` | Exa MCP / REST + `SafeHttpClient`（SSRF 守卫）+ 内存缓存 | `tools_and_security.rs` + `ssrf.rs` 单测（全部离线 Mock） | 多关键词（2–3 词）串行请求，任一失败立即抛错丢弃已完成关键词，与桌面端 `web_search_bar` 的“部分查询失败保留其余关键词”契约不一致；结果仅输出拼接文本 `preview`（1200 字），无结构化标题/URL 列表；关闭系统代理，校园网仅配置 HTTP 代理无 TUN 时无法访问；`web-search.json` 仅允许 `198.18.0.0/15` |
| `source_check` | 与 `web_search` 共用完全相同实现（`web_access.rs:236-247`） | 无独立测试 | 工具名暗示“来源核验”，但实际代码与 `web_search` 完全一致，未做域名权威性校验或交叉核验，容易误导模型与测试预期 |
| `fetch_content` | `SafeHttpClient` + HTML 文本提取 + 内存缓存 | `tools_and_security.rs` 1 项 Mock 测试 | 1–5 个 URL 串行抓取，单个 404/超时即导致整批失败；简易正则/标签剥离对非 UTF-8 编码页面（GBK/GB2312 中文政企站点）与重度 JS 渲染页面未做编码检测与提示 |
| `get_search_content` | 会话内存 `VecDeque` 缓存（32 条 / 8 MiB / 单条 2 MiB / 30 分钟 TTL） | `tools_and_security.rs` 1 项 Mock 测试 | `findText` 使用大小写敏感的 `match_indices`，且零命中时返回空字符串 `""` 而无 `matched: false` 或命中计数；`findText` 模式下仍返回 `offset` 字段（无实际意义）；缓存按会话实例隔离，服务重启或跨会话不可查 |

## 4. 分工具三维度测试用例矩阵（T01–T54）

执行层级约定：**L1** 为 Rust 离线自动测试（CI 必跑）；**L2** 为本机 Windows 真实 API 集成测试（`#[ignore]`，手动或 Windows 专项作业执行）；**L3** 为发布前多机/多系统实机矩阵（含 Win10 1809、家庭中文版、WinPE、慢盘、真实加密卷）。

### 4.1 存储与目录（`read`、`ls`、`disk`）

| 编号 | 工具与场景 | 维度 | 前置与触发输入 | 层级 | 通过判据 |
|---|---|---|---|---|---|
| `T01` | `read` 路径安全与边界 | 兼容 / 语义 | 读取含中文、空格、括号路径；读取 `data/config/auth.json`、`.state/` 硬链接、ADS（`a.txt:stream`）、设备路径（`\\.\PhysicalDrive0`） | L1 | 普通中文路径成功返回；全部受保护路径、硬链接、ADS 与设备路径均返回权限拒绝且不泄露文件内容 |
| `T02` | `read` 编码、锁文件与大图 | 兼容 / 性能 | 读取 20 MiB 大文本（带 `offset`/`limit`）、其他进程共享打开的日志、UTF-16 LE / GBK 日志、PNG/JPEG/WebP/BMP 图片 | L1 / L2 | 文本严守 2000 行 / 50 KiB 上限且内存峰值受控；共享写锁文件可读；非 UTF-8 编码不崩溃并明确记录乱码/处理策略；图片缩放与 MIME 输出符合模型接口要求 |
| `T03` | `ls` 实时目录与卷缓存命中 | 性能 / 语义 | 在同一卷先 `disk(usage)` 再 `ls` 子目录，以及跨卷（如 `C:` 与 `E:`）交替查询 | L2 | 同卷命中 `wiztree-index` 且耗时 < 200 ms；跨卷不串用缓存；`entries` 按 `bytes` 降序，`omitted` 计数与大小之和精确匹配 |
| `T04` | `ls` 单个不可读子项与符号链接 | 兼容 / 语义 | 目录中同时含正常文件、权限拒绝子目录、目录联接（Junction）、悬空符号链接 | L1 / L2 | 单项权限拒绝不中止整次 `ls`（待修复项回归）；链接标记 `unknown` / `bytes: null`；存在不完整项时全部 `pct` 为 `null` 且 `notice` 标明大小为下界 |
| `T05` | `ls` 0 字节文件与缓存一致性 | 语义 | 先缓存 ≥1 MiB 文件，随后将该文件截断为 0 字节再执行 `ls` | L1 | 返回 `bytes: 0`（禁止回退旧缓存中的 ≥1 MiB 历史值） |
| `T06` | `ls` 百万级目录预算与取消 | 性能 | 构造超限条目目录或短预算配置，并测试中途触发 `CancellationToken` | L1 / L2 | 触发条目或时间预算时正常返回 `childrenTruncated: true` 与下界大小；取消后 1 秒内返回错误且不泄漏后台线程 |
| `T07` | `disk(space)` 异构卷与零剩余 | 兼容 / 语义 | 包含满盘（`freeGB: 0`）、空光驱/未就绪读卡器、断连网络映射盘、只读 U 盘 | L1 / L2 | 满盘保留 `freeGB: 0` 与 `usedPct: 100`；不可就绪盘不拖死整次调用，记入 `collectionErrors` 且保留其他可用卷；`freeGB <= totalGB` |
| `T08` | `disk(info)` 盘符过滤与关联失败 | 兼容 / 语义 | 指定普通基础盘盘符、存储空间/VHD 卷盘符，以及关联查询失败场景 | L1 / L2 | 关联成功仅返回对应物理盘与卷；关联失败保留全量 `physicalDisks` 并附 `infoNotice` 与 `degraded: true`；物理盘与卷均失败返回 `isError: true` |
| `T09` | `disk(health)` 权限、多盘与非 SMART 设备 | 兼容 / 语义 | 标准用户与管理员分别查 NVMe、SATA、USB U 盘、虚拟机磁盘；单盘失败与多盘共存 | L2 / L3 | 仅真正权限拒绝时 `smartNotice` 提示管理员权限；U 盘/虚拟盘不支持 SMART 时保留 `smartErrors` 而不把 `null` 表述为健康；成功盘数据不被失败盘吞没 |
| `T10` | `disk(all)` 局部失败与错误去重 | 语义 | 指定 `drive` 且盘符关联失败时调用 `disk(all)` | L1 | `space` 正常返回，`info` 与 `health` 各自保留降级信息，`collectionErrors` 不出现重复关联错误条目 |
| `T11` | `disk(usage)` WizTree 真实导出与非 NTFS | 兼容 / 性能 | 在 NTFS 卷（管理员与非管理员）、FAT32/exFAT U 盘与 UNC 路径执行 `usage` | L2 / L3 | 管理员 NTFS 走 `wiztree-mft` 并产出四类排行；FAT32/exFAT 标 `wiztree-walk`；非管理员或 WizTree 缺失自动降级原生遍历并标 `degradedFrom`；`.state/tmp/wiztree/` 结束零残留 |
| `T12` | `disk(usage)` 超时、取消与 Job Object 回收 | 性能 | 对慢速介质启动 `usage` 后在 2 秒内取消或触发短超时 | L1 / L2 | `WizTree64.exe` 由 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 立即终止，任务管理器无残留进程，临时 CSV 已删除 |
| `T13` | `disk` 单位与 `tool_presenter` 对齐 | 语义 | 使用真实输出喂入 `cst-client::tool_presenter::map_tool` | L1 | 确认 GiB 换算精度（1 位小数）、`sizeGB` / `totalGB` / `freeGB` 与卡片展示文案一致，枚举值展示可读 |
| `T14` | `disk` 换盘缓存失效（U 盘拔插） | 兼容 / 语义 | 同盘符挂载 U 盘 A 扫描后更换为 U 盘 B（不同卷序列号或文件系统）再查 `ls` / `usage` | L3 | 卷身份变化立即清空旧快照与旧失败标记，不返回 U 盘 A 的目录数据 |

### 4.2 系统负载与硬件（`sys`、`startup`）

| 编号 | 工具与场景 | 维度 | 前置与触发输入 | 层级 | 通过判据 |
|---|---|---|---|---|---|
| `T15` | `sys(overview)` 真实 WMI 与字段完整性 | 兼容 / 语义 | 在物理机、组装机（默认 OEM 字符串）、虚拟机运行 `overview` | L2 / L3 | `cpuTotalPct` 在 `0–100`；`mem.usedMB <= totalMB` 且 `usedPct` 与比值一致；`uptime.totalHours >= 0`；BIOS 厂商缺失时仍保留版本号；缺失项为 `null` |
| `T16` | `sys(overview)` 局部与全量 WMI 故障 | 兼容 / 语义 | 分别模拟 1 类 WMI 失败与全部 7 类 WMI 失败（含 WinPE 无 WMI 服务） | L1 / L3 | 局部失败保留其余字段并设 `degraded: true`；全量失败返回 `isError: true` |
| `T17` | `sys(proc)` 双采样精度与边界 | 性能 / 语义 | 高负载多核机执行 `proc(top=15)`，同时包含采样期新建/退出进程与系统进程 | L1 / L2 | 总耗时约 1.3–2.5 秒；`intervalSec` 接近实测间隔；`byCpu` 与 `byMem` 均 ≤15 项；`cpuPct` 处于 `0–100`；无权访问的进程 `path` 为 `null` 而非报错中止 |
| `T18` | `sys(io)` 物理盘与进程口径隔离 | 性能 / 语义 | 连续两次执行 `sys(io)`（冷/热）并叠加顺序文件读写负载 | L2 | `disks` 排除 `_Total` 且 `busyPct` 在 `0–100`；读写进程出现在 `byIo`；耗时 ≤ 4 秒；单侧数据源失败时保留另一侧数据并标 `degraded: true` |
| `T19` | `sys(gpu)` 中英文 PDH 本地化与短命进程干扰 | 兼容 / 性能 | 在中文与英文 Windows 分别运行，期间反复启停短暂 GPU 进程 | L2 / L3 | `PdhAddEnglishCounterW` 正确解析本地化路径；`gpuPct` 取单进程最繁忙单引擎且 ≤ 100；短命进程退出不导致误重试翻倍耗时或虚假 `degraded` |
| `T20` | `sys(gpu)` 单独显、双显卡、纯核显与无显卡 | 兼容 / 语义 | 分别在 NVIDIA 独显、Intel/AMD 核显+NVIDIA 独显、纯核显、WARP/无驱动环境执行 | L2 / L3 | 多独显按 `uuid`/`pciBusId` 区分；缺失 `nvidia-smi` 时正确拉起 `cst-lhm-host.exe` 填充 `lhmGpu`；`System32` 与 `NVSMI` 路径均能发现驱动工具；无显卡环境不崩溃 |
| `T21` | `sys(sensor)` LHM 隔离、ACPI 热区与频率 | 兼容 / 语义 | 测试有/无 `cst-lhm-host.exe`、有/无 PawnIO、无 ACPI 热区的台式机/VM、睿频 >100% 场景 | L1 / L2 / L3 | `cst-lhm-host.exe` 缺失时保留热区与频率并标 `degraded: true`；热区按 `K - 273.15` 转换并过滤 `[-50, 150]` 之外异常值；频率保留 `>100%` 睿频值（`PDH_FMT_NOCAP100`）；LHM 传感器总数 ≤ 200 且全为有限数 |
| `T22` | `sys(sensor/gpu)` LHM 辅助进程异常与超时 | 性能 / 兼容 | 注入挂起或崩溃的模拟 `cst-lhm-host.exe`，验证 20 秒超时与 Job Object 强杀 | L1 / L2 | 超时或崩溃后子进程彻底销毁，主服务不崩溃且返回结构化 `LHM` 错误 |
| `T23` | `startup` 注册表与 `StartupApproved` 三态 | 兼容 / 语义 | 同时存在 `Run` 启用、`Run` 禁用（首字节奇数）、同名 `RunOnce`、空/损坏二进制值、`Wow6432Node\Run` | L1 / L2 | `disabled` 准确返回 `true / false / null`；同名 `RunOnce` 恒为 `null` 不继承 `Run` 状态；缺失或损坏键值不崩溃 |
| `T24` | `startup` 启动目录、服务排序与路径截断 | 兼容 / 语义 | 包含中文文件名快捷方式、自启服务（`Running` 与 `Stopped` 混合）、超 140 字符带空格 `PathName` | L1 / L2 | `Running` 服务排在 `Stopped` 前；超 140 字符 `path` 按 UTF-8 字符边界安全截断加 `...`；双源全失败返回 `isError: true` |
| `T25` | 诊断全局 4 槽位并发与背压 | 性能 | 同时并发发起 6 个诊断调用（含 2 个慢 `ls`/`usage` + 4 个快 `overview`/`startup`）并取消其中 2 个 | L1 / L2 | 槽位不死锁、不创建无界线程；取消的任务立即返回，待慢任务自然结束后槽位完整归还 |

### 4.3 事件日志与设备驱动（`eventlog`、`driver`）

| 编号 | 工具与场景 | 维度 | 前置与触发输入 | 层级 | 通过判据 |
|---|---|---|---|---|---|
| `T26` | `eventlog(recent)` 倒序、聚合与计数一致 | 语义 | 真实 `System` + `Application` 通道查询默认 24 小时 | L2 | `events` 按时间精确倒序；未截断（`!countsTruncated && !scanTruncated`）时 `sum(counts.n) == total`；每条 `msg` ≤ 200 字符 |
| `T27` | `eventlog(boot/crash/service/disk)` 场景过滤器 | 语义 / 兼容 | 真实日志 + 构造多通道事件（含 WHEA、WER 1001 Info、Hang 1002、SCM 7045） | L1 / L2 | `boot` 合并去重后再截 `top`；`crash` 保留 Info 级 1001/1002 且 `app` 过滤覆盖全组；`service` 排除 7045；`disk` 命中全部 10 个存储事件 ID |
| `T28` | `eventlog(security)` 权限边界与错误归因 | 兼容 / 语义 | 标准用户、管理员、`source.admin()` 查询失败三种情况 | L1 / L2 | 标准用户直接返回 `admin: false, degraded: true` 且不查通道；管理员返回真实登录/锁定事件；令牌检查异常时区分“权限检测失败”与“非管理员” |
| `T29` | `eventlog(query/detail)` 参数校验与精确定位 | 兼容 / 语义 | 传入超长 `ids`（如 500 个 ID）、非法通道名、特殊字符正则、`detail(recordId)` 与 `detail(id)`（含日志轮转后已不存在记录） | L1 / L2 | 通道名与超长 `ids` 边界受控不触发未处理 XPath 异常；`detail(id)` 返回该 ID **最新**一条并保留换行（≤ 20,000 字）；不存在记录返回 `found: false` |
| `T30` | `eventlog` 10 万条大通道性能与延迟格式化 | 性能 | 在包含 5 万–10 万条记录的通道运行 `recent` / `query(provider=...)` | L2 | 耗时处于 30 秒预算内（优化延迟 `EvtFormatMessage` 后目标 ≤ 3 秒）；达到 10 万条标记 `scanTruncated: true` 与 `degraded: true`；同秒不同小数位时间戳排序正确 |
| `T31` | `eventlog` 缺失消息 DLL 与服务停用 | 兼容 / 语义 | 通道含已卸载软件的自定义提供程序事件；以及 WinPE/停用 EventLog 服务场景 | L2 / L3 | 缺失提供程序元数据不阻塞其余事件；被过滤掉的事件不污染 `unreadable`；全通道失败返回明确错误（非 `noMatch: true`） |
| `T32` | `driver(problem/find)` 异常设备与 AND 组合查找 | 兼容 / 语义 | 存在正常/异常设备，测试 `class`、`id`（含 `PCI\VEN_...&DEV_...` 特殊字符）、`name + class` 交集及空条件 | L1 / L2 | 空条件报错；`id` 同时匹配 `deviceId` 与 `hardwareIds`；多条件严格 AND；`problem` 区分“无异常设备”与“WMI 失败” |
| `T33` | `driver(core)` 全源失败与局部降级 | 兼容 / 语义 | 模拟 `Win32_PnPSignedDriver` 单类失败，以及全部 5 类 WMI 失败 | L1 | 单类失败保留其余 4 类数据并标 `degraded: true`；全部 5 类失败返回 `isError: true`（校验 `sources == 5` 修复） |
| `T34` | `driver(external)` 现代外设识别 | 兼容 / 语义 | 接入 USB U 盘、USB 移动固态硬盘（PSSD）、蓝牙外设、外接显示器 | L2 / L3 | 核验 `devices` 与 `removable` 是否漏报报告为 `External hard disk media` 的 USB 移动硬盘，并在文档或查询条件中保持一致口径 |

### 4.4 文件检索与联网访问（`fffind`、`ffgrep`、`web_search`、`source_check`、`fetch_content`、`get_search_content`）

| 编号 | 工具与场景 | 维度 | 前置与触发输入 | 层级 | 通过判据 |
|---|---|---|---|---|---|
| `T35` | `fffind` / `ffgrep` 敏感路径与硬链接隐藏 | 语义 / 兼容 | 在工作区创建指向 `data/config/auth.json` 与 `.state/` 的硬链接和目录联接，分别模糊搜索与正则搜索 | L1 | 敏感文件、硬链接与 `.state/` 全部零命中、不报错中断，正常文件可检出 |
| `T36` | `fffind` / `ffgrep` 万级文件性能基准 | 性能 | 在包含 10,000+ 文件的目录执行 `fffind` 与 `ffgrep`（含 `exclude`、取消令牌） | L2 | 耗时 ≤ 3 秒（监控 `PathGuard::check` 系统调用放大问题）；取消后 500 ms 内退出；单次读取总字节 ≤ 256 MiB |
| `T37` | `fffind` / `ffgrep` 跨目录、反斜杠与通配符 | 兼容 / 语义 | 分别传相对路径、绝对路径（跨盘符）、含中文/空格路径、`exclude: ["*.log", "sub/**"]` | L1 | 相对与绝对路径下 `exclude` glob 行为一致；返回的 `path` 统一使用正斜杠且不越权 |
| `T38` | `ffgrep` Windows 常见文本编码与超长行 | 兼容 / 语义 | 检索 UTF-8、UTF-8 BOM、UTF-16 LE、GBK 中文日志、真二进制文件、单行 >8 KiB 压缩日志 | L1 | 真二进制静默跳过；超 8 KiB 行计入 `skipped` 且不 OOM；UTF-16 LE 与 GBK 行为与规范文档一致（若不支持须明确记录或补转码） |
| `T39` | `ffgrep` 上下文、50 KiB 上限与 `truncated` | 性能 / 语义 | 构造大量命中配合 `context: 3`、`limit: 1000` 逼近 50 KiB 输出上限 | L1 | 在 50 KiB 前受控截断并置 `truncated: true`，不会因边界估算偏差反触发硬报错 |
| `T40` | `web_search` / `source_check` 单次与批量关键词 | 语义 / 兼容 | 传 1 个和 3 个关键词（含中文），模拟第 2 个关键词上游 500/超时 | L1 | 单次正常返回 `responseId` 与预览并入缓存；确认批量局部失败策略与 `tool_presenter` / `web_search_bar` 的 UI 预期一致；明确 `source_check` 是否与 `web_search` 保持同构或增加核验字段 |
| `T41` | `web_search` / `fetch_content` SSRF 与重定向红线 | 兼容 / 语义 | 目标分别为回环 IPv4/IPv6、私网段、`169.254.169.254`、IPv4-mapped IPv6、公网 302 跳转内网、HTTPS 降级 HTTP、DNS 混合解析（公网+私网）、`198.18.x.x` TUN 网段 | L1 | 除显式允许的 `198.18.0.0/15` 外全部拦截；带凭据与 POST 请求绝不跟随重定向；重定向链超限终止 |
| `T42` | `web_search` / `fetch_content` 代理与离线环境 | 兼容 | 分别在 `PI_OFFLINE=1`、本机 TUN 代理、仅配置系统 HTTP 代理（无 TUN）、断网环境调用 | L2 / L3 | 离线/断网返回清晰可读网络错误而非 panic；记录系统 HTTP 代理环境下的实际连通边界 |
| `T43` | `fetch_content` 多编码 HTML、体积上限与局部失败 | 兼容 / 性能 | 抓取 UTF-8、GBK 中文页、纯文本、含大段 `<script>`/`<style>`、超 2 MiB 响应、多 URL 中混入 404 | L1 | 脚本样式干净剔除；超限按字节边界截断不切碎多字节 UTF-8；记录 GBK 解码与批量失败口径 |
| `T44` | `get_search_content` 分页、中文定位与缓存淘汰 | 性能 / 语义 | 连续写入 33 条或总计 >8 MiB 触发 LRU/FIFO 淘汰；中文字符 `offset`/`limit` 分页；`findText` 大小写、多处命中与零命中 | L1 | 淘汰后旧 `responseId` 返回过期错误；中文分页无半字乱码且不超过 6000 字符；核验 `findText` 大小写与零命中语义 |

### 4.5 命令方案与受控执行（`command_plan`、`runbook_deliver`、`runbook`、`bitlocker`）

| 编号 | 工具与场景 | 维度 | 前置与触发输入 | 层级 | 通过判据 |
|---|---|---|---|---|---|
| `T45` | `command_plan` 版本锁、并发修订与防覆盖 | 语义 / 性能 | 新建方案、合法修订（递增 `version`）、旧 `baseVersion` 冲突、两请求并发修订同一 `runbookId` | L1 | 仅一次并发修订成功；冲突报错且不破坏已保存版本；损坏的最新版本仅阻断自身 ID |
| `T46` | `runbook_deliver` 幂等复用、篡改保护与编码 | 兼容 / 语义 | 交付最新版本、跨时区重试交付、人工修改已导出 TXT 后再交付、尝试交付旧版本 | L1 | 输出文件为 UTF-8 BOM + CRLF 且每条命令格式合规；同版本未改直接复用；现场修改过或过期版本拒绝覆盖 |
| `T47` | `runbook` 序号边界、覆盖清理与只读介质 | 兼容 / 语义 | 同日连续写入、指定已有 `sequence` 改标题替换、序号达 `999`、只读目录/写保护 U 盘、中文长标题含非法路径字符 | L1 / L3 | 序号唯一递增，替换后旧标题文件清理干净且不留临时文件；满 999 明确拒绝；只读介质清晰报错无残留 |
| `T48` | 命令方案 50 KiB 模型文本裁剪风险 | 语义 | 构造 20 条各 2000 字符命令的满载方案，分别检查 `content` 与 `details` | L1 | 验证总字节是否超过 50 KiB；若超限，确认 `file` 路径与关键命令字段不被破坏性截断或明确收窄单条上限 |
| `T49` | `runbook` 与 `command_plan` 字段契约一致性 | 语义 | 对比三工具返回结果经 `tool_presenter::map_tool` 后的卡片投影 | L1 | 验证 `commands[].shell`（`"powershell"` vs `"PowerShell"`）、`levelLabel`、`action` 在桌面端和 Web 端卡片均可无损渲染 |
| `T50` | `bitlocker(status)` 异构系统与指定盘符错误归因 | 兼容 / 语义 | 在 Win11 Pro、Win10/11 家庭中文版、标准用户、WinPE 下分别调用 `status`（全卷与指定 `drive="C:"`） | L1 / L2 / L3 | 命名空间缺失或权限不足时，指定 `drive` 仍优先透出 `collectionErrors` 真实原因而非笼统报“未找到唯一卷”；绝不把未枚举卷报为未加密 |
| `T51` | `bitlocker(unlock)` 隐私三条边界与内存清零 | 语义 | 本地隐藏输入与 AI 填参两条路径；覆盖流式单字节分片、退格/取消/超时、强杀重启 | L1 / L2 | 48 位恢复密码绝不进入 JSONL、`ExecutionStateStore`、SSE 事件、遥测与日志；`BSTR` 与 `SecretInput` 使用后逐单元清零 |
| `T52` | `bitlocker(unlock/decrypt)` 真实错误码与无自动修复 | 兼容 / 语义 | 模拟及实机加密卷测试：错误密码、锁定卷解密（`0x80310000`）、系统卷自动解锁依赖（`0x80310029`）、已完成/进行中卷 | L1 / L3 | 原样返回 HRESULT 错误码，不自动解锁或删除保护器；`encryptionPercentage: 0` 不误判为解密完成，仅 `FullyDecrypted` 为完成 |
| `T53` | `bitlocker` 同卷跨进程互斥与中断复查 | 性能 / 语义 | 双进程并发操作同卷与异卷；操作提交后触发超时/取消/进程崩溃重启 | L1 / L2 | 同卷互斥拒绝、异卷不阻塞；中断后标记 `operation.status: "unknown"`，重启后仅执行只读实时复查且绝不自动重放命令 |
| `T54` | 17 工具 Schema / `SKILL.md` / `tool_presenter` 四方契约回归 | 语义 | 遍历 `OPEN_TOOL_NAMES` 生成的 JSON Schema、示例输出、8 份 `app/skills/*/SKILL.md` 与 `tool_presenter.rs` | L1 | 17 工具名称、必需参数、枚举值、顶层返回键与前端消费字段 100% 对齐，杜绝静默字段名漂移 |

## 5. 分层执行建议

1. **L1（CI 必跑）**：在 `crates/cst-kit/tests/` 补齐针对上述源码缺陷的纯 Rust 回归测试（不依赖真实硬件与外网），并在 `crates/cst-client/tests/workspace_and_tools.rs` 补充 17 工具四方契约校验（`T54`）。
2. **L2（本机 Windows 真环境套件）**：在 `crates/cst-kit/tests/` 新增 `live_windows_tools.rs`（标记 `#[ignore]`，仅 `cfg(windows)` 编译），通过单条命令在本机验证真实 WMI、PDH、Event Log、注册表、`WizTree64.exe`、`cst-lhm-host.exe` 与 `BitLocker(status)` 的返回结构与耗时门槛：
   ```powershell
   cargo test -p cst-kit --test live_windows_tools --locked -- --ignored --nocapture
   ```
3. **L3（发布前实机矩阵）**：将 `T09`、`T11`、`T14`、`T15`、`T19`、`T20`、`T21`、`T31`、`T34`、`T42`、`T47`、`T50`、`T52` 纳入 [TESTING.md · 实机环境覆盖矩阵](../../test/TESTING.md#实机环境覆盖矩阵) 与 [windows-compat.md](../../desktop/test/windows-compat.md)，逐环境留存 JSON 原始输出与耗时记录。

## 6. 源码审计修复进展与待决事项

### 6.1 明确缺陷修复状态

2026-11-23 已完成全部明确 BUG 修复与 L1/L2 回归验证（`live_windows_tools.rs` 8 组全过，`cst-kit` / `cst-client` L1 回归全过）：

| 优先级 | 位置 | 缺陷与处置结果 |
|---|---|---|
| P1 | `crates/cst-kit/src/diagnostics/driver.rs` | **已修**：`driver(core)` 全失败判定修正为 5 次查询 |
| P1 | `crates/cst-kit/src/bitlocker/core.rs` | **已修**：指定 `drive` 且卷列表为空时优先透出 `collectionErrors` 真实原因与脱敏错误码 |
| P1 | `crates/cst-kit/src/diagnostics/win_native/windows/events.rs`、`eventlog.rs` | **已修**：延迟 `EvtFormatMessage` 至最终 `top` 或消息筛选候选，消除 10 万条提前格式化与误计 `unreadable` |
| P1 | `crates/cst-kit/src/tools/fff.rs`、`path_guard.rs` | **已修**：遍历阶段先按路径条件筛候选再做文件级 `PathGuard` 校验；同步修复 Windows 扩展路径 `cwd` 导致相对 glob 漏匹配问题（注：2,500 文件 `ffgrep` 仍耗时约 5.2 秒，后续可进一步缓存只读会话级保护句柄） |
| P1 | `crates/cst-kit/src/diagnostics/walk.rs` | **已修**：`walk::usage` 入口补充目录存在性与重解析点校验，不存在路径直接报错 |
| P2 | `crates/cst-kit/src/diagnostics/hardware/pdh.rs`、`sys_load.rs` | **已修**：GPU 存在有效引擎样本时不误触发二次重采；仅引擎无样本时单重采引擎并保留首轮显存数据 |
| P2 | `crates/cst-kit/src/diagnostics/win_native/windows.rs`、`mod.rs` | **已修**：同次诊断阻塞任务内复用 COM、Tokio 运行时与同命名空间 WMI 连接，结束即释放 |
| P2 | `crates/cst-kit/src/diagnostics/ls.rs` | **已修**：单项 `read_dir` 失败跳过并标不完整；0 字节文件使用实时元数据；子目录回退遍历改标 `walk` |
| P2 | `crates/cst-kit/src/diagnostics/disk.rs`、`scan.rs` | **已修**：`disk(all)` 盘符关联失败去重；`wiztree-mft` 仅在确认提权且 NTFS 时标注 |
| P2 | `crates/cst-kit/src/diagnostics/hardware.rs` | **已修**：`nvidia-smi.exe` 优先 `System32`，回退至已知 `Program Files\NVIDIA Corporation\NVSMI` 路径并拒绝链接 |
| P2 | `crates/cst-kit/src/diagnostics/sys.rs` | **已修**：`proc` 的 `cpuPct` 钳制至 `0–100`；`overview` 在 BIOS 厂商为空时保留版本号 |
| P2 | `crates/cst-kit/src/diagnostics/eventlog.rs` | **已修**：区分权限检测失败与非管理员；`query.ids` 上限锁定为 16 项 |
| 待定 | `lhm/host/cst-lhm-host.exe` | **暂留至依赖评审**：开发仓库按 `lhm/README.md` 不存二进制，运行时已正确降级并保留热区与频率数据 |

### 6.2 待拍板的设计议题

1. **WizTree 授权与替代路线**：`WizTree64.exe` 属于个人免费、商用需购授权的专有软件，且当前仍随包打包无用的 32 位 `WizTree.exe`（7.3 MB）与 60+ 语言包。需确认保留 `WizTree64.exe` 精简打包（剔除 32 位与 `locale/`），或完全以 Rust 原生 MFT/遍历替代。
2. **`web_search` 与 `source_check` 定位及批量失败策略**：两者当前实现完全相同，且 2–3 个关键词任一失败即整批报错（与桌面端 UI 设计的局部失败展示不一致）。需确认将多词改为逐项容错返回，并决定保留 `source_check` 作为差异化核验工具或合并精简。
3. **`ffgrep` 对 Windows 常见非 UTF-8 编码的支持**：现场排查常需检索 GBK 中文日志与 UTF-16 LE PowerShell/DISM 日志，当前一律按 UTF-8 读取或判为二进制跳过。需确认是否引入编码探测（UTF-16 BOM + GB18030 回退）以提升诊断实用性。
