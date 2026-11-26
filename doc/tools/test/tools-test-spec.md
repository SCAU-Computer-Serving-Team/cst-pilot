# 14 个开放工具评估与三维度测试规范

本文面向 `crates/cst-kit` 开放的 14 个白名单工具，记录当前实现依赖、测试覆盖口径与三维度验证矩阵（**兼容性**、**性能**、**字段语义正确性**）。接口契约见 [API.md](../API.md)，公共实机核查组（B01–B35）与自动化入口见 [TESTING.md](../../test/TESTING.md)。

## 1. 三项测试维度与验收门槛

| 维度 | 核心覆盖面 | 统一验收红线 |
|---|---|---|
| **兼容性** | Windows 10 1809–Windows 11 25H2、家庭中文版、精简镜像、WinPE；管理员与标准用户；中英文系统语言与 GB18030 / UTF-16 日志；NVMe/SATA/USB/动态盘/光驱/断连网络盘；独显/核显/双显卡/无显卡；TUN 与 HTTP 代理、离线模式 | 受限或缺失接口禁止崩溃或静默假成功；权限不足、服务停用、命名空间缺失与无匹配区分呈现；中文路径、空格、括号与本地化性能计数器路径正常工作 |
| **性能** | 冷/热启动耗时、原生 Win32/NT/PDH/EvtRender/文件遍历吞吐、内存与读取上限、全局 4 槽位并发隔离、取消响应时间、`50 KiB` 输出上限 | 常规只读诊断 P95 ≤ 5 秒（双采样 `proc`/`io`/`gpu`/`sensor` ≤ 3 秒）；取消后 1 秒内返回且不泄漏孤儿进程；大目录与大日志在预算边界内收口，单工具慢调用不拖死会话主循环 |
| **字段语义正确性** | 统一整数毫秒 `elapsedMs`；单位（GiB/MiB/KiB/s/°C）与字段名一致；三态语义（`null` / `0` / `[]`）；`degraded`、`noMatch`、`isError` 边界；空错误容器省略；`50 KiB` 受保护键裁剪；脱敏与四方契约（Schema / 实现 / `tool_presenter` / `map-tool`） | 缺失值统一为 `null`，不以 `0` 或空数组冒充正常；正常调用不出现空 `collectionErrors` / `counterErrors`；全源失败返 `isError: true`；`50 KiB` 裁剪保留合法 JSON 与核心交付字段；凭据与恢复密码零外泄 |

## 2. 14 个开放工具实现与测试口径总表

| 工具 | 核心实现与系统依赖 | 自动化测试覆盖 | 待补实机验证边界 |
|---|---|---|---|
| `read` | `PathGuard` 单句柄核验 + 文本转义余量扫描 + 图片处理器（`[Text(JSON), Image]`） | `read.rs` 单测 + `tools_and_security.rs` + 双端卡片单测 | 共享写锁日志、超大文件与非 UTF-8 编码实机复查 |
| `ls` | `VolumeIndex`（`ntfs-ng` `$MFT` 缓存）+ 无条目上限并行 Win32 `walk.rs` | `storage_scan.rs` + `live_windows_tools.rs` | 断连 UNC 槽位恢复、4Kn、BitLocker 两态与热拔插 |
| `disk` | Win32 `GetDiskFreeSpaceExW` + 存储 IOCTL（NVMe/ATA/盘符映射/SetupAPI）+ WMI 回退 + `$MFT` + 并行 `walk.rs` | `diagnostics_contract.rs` + `storage_scan.rs` + `live_windows_tools.rs` | SATA/USB-SAT/RST 控制器、断连 SMB、4Kn、BitLocker 锁定卷 |
| `fffind` | `ignore` 并行遍历（上限 8 线程）+ `globset` + 相关性打分 + `PathGuardSession` | `fff.rs` 单测 + `tools_and_security.rs` | 十万级超大目录实机基准 |
| `ffgrep` | `ignore` 并行遍历 + `memchr` + `regex::bytes` + `encoding_rs`（UTF-16 / GB18030）+ `256 MiB` 降级保护 | `fff.rs` 单测 + `tools_and_security.rs` | 超大日志目录并发取消回归 |
| `sys` | `NtQuerySystemInformation(5)` + Win32 原生 `overview` + PDH（含路径缓存与 `PhysicalDisk`）+ `Toolhelp32` + `nvidia-smi` + 并行双卡 `cst-lhm-host.exe` | `diagnostics_contract.rs` + `sys_load.rs` + `live_windows_tools.rs` + `lhm-host/test.ps1` | 物理双显卡（核显+独显）实机、英文 Windows、WinPE |
| `startup` | Win32 注册表 + 启动文件夹 + Win32 SCM（WMI 回退） | `diagnostics_contract.rs` + `live_windows_tools.rs` | 损坏 `StartupApproved` 二进制、WinPE 环境 |
| `eventlog` | Win32 `EvtQuery` / `EvtNext(64)` / `EvtRenderEventValues` / 延迟 `EvtFormatMessage` + 本机偏移 RFC3339 | `diagnostics_contract.rs` + `events.rs` 单测 + `live_windows_tools.rs` | 10 万条大通道、缺失消息 DLL、EventLog 服务停用 |
| `driver` | WMI PnP + Win32 SCM（核心服务）+ SetupAPI（四类签名驱动）+ `YYYY-MM-DD` 日期格式化 | `diagnostics_contract.rs` + `live_windows_tools.rs` | USB PSSD、蓝牙外设、多厂商虚拟驱动实机 |
| `bitlocker` | COM/WMI `PacketPrivacy` + `Win32_EncryptableVolume` + `Zeroizing` + 命名管道同卷锁 | `bitlocker_privacy.rs` + `bitlocker_execution.rs` + `api_contract.rs` + `tools_and_bitlocker.rs` | 真实加密卷解锁/解密、家庭中文版与 WinPE |
| `web_search` | Exa（免费 MCP 回退 + 付费 REST `fast`/`auto`/`deep`）+ DeepSeek `/v1/tools/search` + `SafeHttpClient`（SSRF）+ 并发局部降级 | `web_access.rs` 单测 + `ssrf_tests.rs` + `tools_and_security.rs` + `api_contract.rs` | 校园网 HTTP 代理、TUN 假 IP 与离线环境联调 |
| `web_fetch` | `SafeHttpClient` + `auto`/`provider`/`direct` 抓取 + UTF-8/GB18030 解码 + 并发局部降级 | `web_access.rs` 单测 + `ssrf_tests.rs` + `tools_and_security.rs` | 重度 JS 站点、政企 GB18030 站点与大体积页面 |
| `get_search_content` | 会话内存缓存（32 条 / 8 MiB / 单条 2 MiB / 30 分钟 TTL）+ 中文分页与定位 | `web_access.rs` 单测 + `tools_and_security.rs` | 跨会话与重启后过期提示体验 |
| `runbook` | 单工具 `plan`/`export` + 当天序号 `01–999` + 命名管道目录锁 + UTF-8 BOM/CRLF + 服务端一键导出接口 | `runbook_and_plan.rs` + `runbook_session.rs` + `api_contract.rs` + `tools_and_bitlocker.rs` | 只读 U 盘介质、中文长标题与跨天序号重置实机 |

## 3. 分工具三维度测试用例矩阵（T01–T50）

执行层级：**L1** 为 Rust/Node 离线自动测试（CI 必跑）；**L2** 为本机 Windows 真实 API 集成测试（`#[ignore]`，手动或 Windows 专项作业执行）；**L3** 为发布前多机/多系统实机矩阵（含 Win10 1809、家庭中文版、WinPE、双显卡与真实加密卷）。

### 3.1 文件与存储（`read`、`ls`、`disk`、`fffind`、`ffgrep`）

| 编号 | 工具与场景 | 维度 | 前置与触发输入 | 层级 | 通过判据 |
|---|---|---|---|---|---|
| `T01` | `read` 路径安全与 JSON 结构 | 兼容 / 语义 | 读取含中文/空格路径；读取受保护凭据、`.state/` 硬链接、ADS 与设备路径 | L1 | 普通路径返回含 `elapsedMs` 与 `kind: "text"` 的单块 JSON；全部受保护路径与链接返回 `isError: true` 且不泄露内容 |
| `T02` | `read` 大文本截断与图片双块 | 性能 / 语义 | 读取超 2000 行或超 `50 KiB` 文本，以及 PNG/JPEG/WebP 图片 | L1 / L2 | 文本严守 `50 KiB`（含 JSON 转义）且不含无效 `bash sed` 提示；图片返回 `[Text(meta_json), Image]` 且 `details` 不含 base64 |
| `T03` | `ls` 卷缓存命中与回退降级 | 性能 / 语义 | 同卷先 `disk(usage)` 再 `ls`；跨卷交替查询；模拟 `$MFT` 失败回退 | L1 / L2 | 同卷命中 `native-index` 且耗时 < 200 ms；跨卷不串用；`$MFT` 失败回退附 `degraded: true` 与 `degradedFrom` |
| `T04` | `ls` 不可读子项与重解析点 | 兼容 / 语义 | 目录含正常文件、权限拒绝子目录、目录联接与悬空符号链接 | L1 / L2 | 单项权限拒绝不中止整次 `ls`；链接标记 `unknown` / `bytes: null`；不完整时全部 `pct` 为 `null` 且 `degraded: true` |
| `T05` | `ls` 0 字节文件与缓存一致 | 语义 | 缓存 ≥1 MiB 文件后截断为 0 字节再执行 `ls` | L1 | 返回 `bytes: 0`，不复用旧缓存值 |
| `T06` | `ls` 超大目录取消与槽位回收 | 性能 | 对大目录触发 `CancellationToken` | L1 / L2 | 取消后 1 秒内返回且不泄漏后台线程与槽位 |
| `T07` | `disk(space)` 异构卷与零剩余 | 兼容 / 语义 | 含满盘（`freeGB: 0`）、空光驱、断连网络盘、只读 U 盘 | L1 / L2 | 满盘保留 `freeGB: 0` 与 `usedPct: 100`；不可就绪盘记入 `collectionErrors` 且不拖死其他卷 |
| `T08` | `disk(info)` 盘符过滤与关联失败 | 兼容 / 语义 | 指定基础盘、存储空间卷，及盘符关联失败场景 | L1 / L2 | 关联成功仅返回对应物理盘与卷；关联失败保留全量 `physicalDisks` 并附 `infoNotice` 与 `degraded: true` |
| `T09` | `disk(health)` 原生 IOCTL 与回退 | 兼容 / 语义 | 标准用户与管理员查 NVMe、SATA、USB U 盘与多盘共存 | L2 / L3 | 序列号核验后输出 6 项字段；仅权限拒绝提示管理员；单盘失败不吞没成功盘 |
| `T10` | `disk(all)` 错误去重与字段齐全 | 语义 | 指定 `drive` 且盘符关联失败时调用 `disk(all)` | L1 | `space` 正常，`info` 与 `health` 保留降级信息，`collectionErrors` 无重复 |
| `T11` | `disk(usage)` `$MFT` 与 `filesScanned` | 兼容 / 性能 | 管理员本地 NTFS 根目录、子目录、非 NTFS 卷与 UNC | L2 / L3 | `native-mft` 与 `native-walk` 均输出 `stats.filesScanned` 且不含 `elapsedSec`；失败回退附 `degradedFrom` 与 `degraded: true` |
| `T12` | `disk` 换盘缓存失效与取消 | 兼容 / 性能 | 同盘符热拔插更换 U 盘；慢盘中途取消 | L1 / L3 | 卷身份变化立即清空旧缓存；取消后 `CancelSynchronousIo` 立即退出且零临时文件 |
| `T13` | `fffind` / `ffgrep` 安全边界与相关性 | 兼容 / 语义 | 含敏感路径硬链接、目录联接、跨盘符 `exclude`、空格多词 | L1 | 敏感文件零命中；`fffind` 精确文件名排首位；跨盘正斜杠通配一致 |
| `T14` | `ffgrep` 多编码、大文件与 `256 MiB` 降级 | 性能 / 语义 | UTF-16 LE/BE BOM、GB18030、二进制、>10 MiB 大文件、触达 `256 MiB` 总上限与 `50 KiB` 输出上限 | L1 / L2 | 多编码正确命中；>10 MiB 跳过记入 `skippedLargeFiles`；触达 `256 MiB` 保留已有命中并置 `truncated: true`、`degraded: true` |

### 3.2 系统诊断（`sys`、`startup`、`eventlog`、`driver`）

| 编号 | 工具与场景 | 维度 | 前置与触发输入 | 层级 | 通过判据 |
|---|---|---|---|---|---|
| `T15` | `sys(overview)` 原生 Win32 与短窗双采 | 性能 / 语义 | 物理机、组装机与虚拟机执行 `overview` | L1 / L2 / L3 | 耗时 ≈ 210–350 ms（消除冷启动近 8 秒 WMI 阻塞）；`cpuTotalPct` 在 `0–100`；内存、内核池与开机时长单位正确；缺失 BIOS 厂商保留版本号 |
| `T16` | `sys(overview)` 局部与全量故障 | 兼容 / 语义 | 模拟原生与 WMI 单类失败及全部失败 | L1 / L3 | 局部失败置 `degraded: true`；全部失败返回 `isError: true` |
| `T17` | `sys(proc)` `NtQuerySystemInformation` 全覆盖 | 性能 / 语义 | 普通权限多核主机执行 `proc(top=15)` | L1 / L2 | 总耗时 ≈ 1.25 s；进程覆盖率 100%（含系统与受保护进程）；按 `(pid, created)` 防复用；仅 Top-N 填充 `path`；`cpuPct` ∈ `0–100` |
| `T18` | `sys(io)` 共享快照与 PDH 同窗 | 性能 / 语义 | 叠加顺序读写负载执行 `sys(io)` | L1 / L2 | 物理盘 PDH 与进程双采重叠在同一 `1.2 s` 窗口，总耗时 ≈ 1.3–1.6 s；排除 `_Total`；单侧失败降级保留另一侧 |
| `T19` | `sys(gpu)` `Toolhelp32`、零负载过滤与空错误省略 | 性能 / 语义 | 中英文 Windows 运行，含空闲引擎与短命 GPU 进程 | L1 / L2 / L3 | `Toolhelp32` 进程名 < 25 ms；`engines[]` 仅含 `gpuPct > 0` 项；无计数器错误时省略 `counterErrors`；PDH 路径缓存复用 |
| `T20` | `sys(gpu)` 双显卡并行核显与单位字段 | 兼容 / 语义 | 单独显、核显+独显双卡、纯核显、LHM 失败 | L1 / L2 / L3 | 检测到非 NVIDIA 物理显卡时并行采集 `lhmGpu` 且不丢失 `nvidia`；`sensors[]` 均含 `unit`；LHM 失败写入 `lhmGpu.error` |
| `T21` | `sys(sensor)` LHM 隔离、单位与频率 | 兼容 / 语义 | 有/无 `cst-lhm-host.exe`、睿频 >100%、辅助进程超时 | L1 / L2 / L3 | `sensors[]` 附 `unit`（上限 200）；热区保留 `[-50, 150]`°C；频率保留 `>100%`；正常省略 `counterErrors`；子进程超时由 Job Object 销毁 |
| `T22` | `startup` SCM 提速与禁用三态 | 性能 / 语义 | 含 `Run` 启用/禁用、同名 `RunOnce`、超 140 字服务路径 | L1 / L2 | SCM 枚举自启服务 < 150 ms（0 失败）；`RunOnce` 不继承 `Run` 禁用；超长 `path` 以 `…` 截断 |
| `T23` | `eventlog` `EvtRenderEventValues` 与本机时区 | 性能 / 语义 | 真实 `System` + `Application` 通道查询 | L1 / L2 | `events[].time` 为本机带偏移 RFC3339（与 `bootTime` 对齐）；`EvtNext(64)` 延迟格式化；`provider` 本地正则匹配与原始 `FILETIME` 排序 |
| `T24` | `eventlog` 场景过滤、详文截断与全败错误 | 兼容 / 语义 | `boot`/`crash`/`service`/`disk`/`security`/`detail` 超 2 万字、全通道失败与单通道失败 | L1 / L2 | `detail` 超 20,000 字附 `msgTruncated: true` 与 `originalChars`；`security` 非管理员返 `degraded: true`；全部通道失败返 `isError: true`，部分失败返 `degraded: true` |
| `T25` | `driver(core)` SetupAPI/SCM 与日期格式 | 性能 / 语义 | 真实 `driver(core)` 及 5 路全失败/单类失败 | L1 / L2 | `driver(core)` 耗时压至 1 秒内；`drivers[].date` 统一输出 `YYYY-MM-DD`；5 路全败返 `isError: true` |
| `T26` | `driver(problem/external/find)` 条件组合 | 兼容 / 语义 | 空条件、`name + class + id` AND 组合、外接 USB 设备 | L1 / L2 / L3 | 空条件报错；`id` 同时匹配 `deviceId` 与 `hardwareIds` |
| `T27` | 全局 4 槽位并发与背压 | 性能 | 并发发起 6 个诊断调用并取消其中 2 个 | L1 / L2 | 取消任务立即返回，槽位不死锁、不泄漏 |

### 3.3 联网检索（`web_search`、`web_fetch`、`get_search_content`）

| 编号 | 工具与场景 | 维度 | 前置与触发输入 | 层级 | 通过判据 |
|---|---|---|---|---|---|
| `T28` | `web-search.json` v1/v2 迁移与凭据安全 | 兼容 / 语义 | 旧 v1 配置加载；`GET/PATCH /api/settings/web-search` 与 `test` 探测 | L1 | 默认 `exa`（开启免费回退）；接口仅返 `hasApiKey`/`apiKeyTail`；`baseUrl` 仅 HTTPS 且换域禁继承旧 Key；`clearApiKey: true` 清空密钥，总大小 `≤ 64 KiB` |
| `T29` | Exa 免费回退与付费 REST 参数守卫 | 兼容 / 语义 | 无 Key 走 MCP；有 Key 走 `fast`(`lite`)/`auto`/`deep` | L1 | `lite` 模式自动剥离不支持参数不触发 `400`；常规与 `deep` 分别使用独立超时 |
| `T30` | DeepSeek Search/Fetch 守卫与复用凭据 | 兼容 / 语义 | >128 字符查询 + 多域名/日期算子；`reuseModelApiKey: true` | L1 | 128 字符守卫保留主查询词、多域名组合为 `(site:a OR site:b)`；`reuseModelApiKey` 仅限官方 `https://api.deepseek.com` |
| `T31` | 多词/多链并发与局部降级 | 性能 / 语义 | 3 个搜索词或 5 个 URL 中 1 条失败；全部失败 | L1 | 并发执行保持输入顺序；局部失败保留成功项并置 `degraded: true` + `collectionErrors`；全部失败返 `isError: true` |
| `T32` | SSRF 阻断、DNS 预检与重定向红线 | 兼容 / 语义 | 回环、私网、`169.254.169.254`、302 跳转内网、`198.18.x.x`、云端抓取前 DNS 预检 | L1 | 除显式放行的 TUN 网段外全部拦截，私网 IP 与内网域名均不外发至 Exa/DeepSeek 云端接口 |
| `T33` | `web_fetch` GB18030 解码与脚本清理 | 兼容 / 语义 | UTF-8 与 GB18030 中文页、含 `<script>`/`<style>`、二进制响应 | L1 | GB18030 中文正确解码，二进制明确拒绝，脚本样式清理干净 |
| `T34` | `get_search_content` 分页、定位与淘汰 | 性能 / 语义 | 超过 32 条或 8 MiB 触发淘汰；中文 `offset`/`limit` 与 `findText` | L1 | 淘汰旧条目返过期提示；中文分页无半字截断；零命中返 `matchCount: 0` |

### 3.4 命令方案、BitLocker 与跨端契约（`runbook`、`bitlocker`、全量契约）

| 编号 | 工具与场景 | 维度 | 前置与触发输入 | 层级 | 通过判据 |
|---|---|---|---|---|---|
| `T35` | `runbook` 计划、修订乐观锁与序号继承 | 语义 / 性能 | 新建方案、合法修订、旧 `baseVersion` 冲突、并发修订 | L1 | 修订继承原 `sequence` 并递增 `version`；并发冲突仅一笔成功；`commands[].shell` 统一小写 |
| `T36` | `runbook` 导出命名、幂等与篡改保护 | 兼容 / 语义 | 导出至 `01-风险档-标题-v1.txt`、同版本复用、人工修改后重导、只读介质 | L1 / L3 | UTF-8 BOM + CRLF；无 UUID 子目录；现场修改或过期版本拒绝覆盖；只读介质清晰报错 |
| `T37` | 服务端与双端卡片一键导出 `.txt` | 语义 / 性能 | `POST /api/sessions/:id/runbook/export` 及双端卡片点击导出 | L1 / L2 | 本地接口 `< 10 ms` 落盘并打开记事本，卡片就地切换为已导出状态，不消耗模型轮次 |
| `T38` | `50 KiB` 受保护键裁剪与 `elapsedMs` | 语义 | 20 条满载长命令与超大诊断数组、工具错误与图片双块 | L1 | 全部输出含整数 `elapsedMs`；超 `50 KiB` 时核心交付键（`file`、`runbookId`、`version`、`elapsedMs` 等）完整保留 |
| `T39` | `bitlocker(status)` 异构系统与空错误省略 | 兼容 / 语义 | Win11 Pro、家庭中文版、标准用户、WinPE，全卷与指定 `drive` | L1 / L2 / L3 | 正常不输出空 `collectionErrors`（零假降级）；指定 `drive` 遇权限或命名空间错误优先透出真实原因 |
| `T40` | `bitlocker(unlock/decrypt)` 隐私、锁与中断复查 | 语义 / 兼容 | 隐藏输入与 AI 填参、同卷并发锁、错误码 `0x80310000`/`0x80310029`、崩溃重启 | L1 / L2 / L3 | 恢复密码零落盘零外泄且内存清零；同卷互斥；中断后标 `unknown` 并在重启后仅执行只读复查 |
| `T41` | 14 工具四方契约与历史工具兼容 | 语义 | `OPEN_TOOL_NAMES`（14 项）、`tool_presenter.rs`、`map-tool.ts` 及旧会话历史（`command_plan`/`runbook_deliver`/`source_check`/`fetch_content`） | L1 | 14 工具名称、字段、耗时回退与双端卡片投影 100% 对齐，旧历史会话只读渲染无回退 |

## 4. 分层执行入口

1. **L1（CI 必跑）**：
   ```powershell
   cargo test --workspace --locked
   npm run lint --prefix src/web/frontend && npm run typecheck --prefix src/web/frontend && npm test --prefix src/web/frontend
   node --test src/test/*.test.mjs
   ```
2. **L2（本机 Windows 真环境套件）**：
   ```powershell
   cargo test --release -p cst-kit --test live_windows_tools --locked -- --ignored --nocapture
   ```
3. **L3（发布前实机矩阵）**：将 `T09`、`T11`、`T12`、`T15`、`T19`、`T20`、`T21`、`T26`、`T36`、`T39`、`T40` 纳入 [TESTING.md · 实机环境覆盖矩阵](../../test/TESTING.md#实机环境覆盖矩阵) 与 [windows-compat.md](../../desktop/test/windows-compat.md)，逐环境留存 JSON 原始输出与耗时记录。
