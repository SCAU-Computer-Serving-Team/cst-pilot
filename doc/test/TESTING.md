# 测试与验收规范

本文定义项目的自动化检查入口、测试隔离原则，以及诊断工具核查组（B01–B35）与实机环境验收要求。运行日志、截图与临时状态统一写入仓库外临时目录（`E:/tmp/<YYYY-MM-DD>/` 或 `F:/tmp/<YYYY-MM-DD>/`）。

## 自动检查与测试入口

以下命令均从仓库根目录执行。Rust 工作区检查与前端静态检查为主线必跑项。`agent` 与 `pack` 下的 TypeScript 扩展检查在阶段 11 清理旧栈前保留为过渡回归入口。

| 范围 | 命令 | 验证职责 |
|---|---|---|
| Rust 格式与编译 | `cargo fmt --all --check`、`cargo check --workspace --all-targets --locked` | Cargo 工作区格式与全部编译目标 |
| Rust 静态检查 | `cargo clippy --workspace --all-targets --locked -- -D warnings` | 全部 Rust 编译目标零警告 |
| Rust 核心与全局回归 | `cargo test --workspace --locked` | 模型协议（`pi-ai`）、Agent 循环（`pi-agent`）、会话存储与扩展（`pi-session`）、运行时 SDK（`pi-runtime`）、业务包与后台服务全量隔离测试 |
| Rust 存储故障注入 | `cargo test --workspace --locked --features test-support`、`cargo clippy --workspace --all-targets --locked --features test-support -- -D warnings` | 开启故障注入特性，验证部分写入、同步失败与尾部恢复保护 |
| Rust 业务与安全专项 | `cargo test -p cst-kit --locked` | 17 工具白名单、凭据与硬链接拦截、SSRF 与 Exa 检索、原生诊断字段与 50 KiB 裁剪、方案版本与 BOM+CRLF 清单、卷身份缓存、GPU/I/O/传感器采集、CSTOA 双路由登录与额度缓存 |
| Rust 遥测专项 | `cargo test -p cst-kit --test telemetry_collect --test telemetry_storage --test telemetry_network --test telemetry_session --locked` | 静默开关、Ed25519 签名、请求扣点有界查询、双命名管道存活证明、目录锁、草稿封存恢复、双端独立队列与共享 5 秒关闭预算 |
| Rust BitLocker 专项 | `cargo test -p cst-kit --test bitlocker_privacy --test bitlocker_execution --locked`、`cargo test -p pi-session --test data_policy --locked` | 恢复密码三条隐私边界、BSTR 清零、本地隐藏输入与确认、同卷命名管道互斥、超时与崩溃后实时状态复查 |
| Rust 软件内升级专项 | `cargo test -p cst-updater --locked`、`cargo test -p cst-kit --test update_package --test update_health_check --test update_service --locked` | 双重签名与 ZIP 边界校验、配置合并、逐文件中断恢复、真实 Windows Job Object 15 秒健康检查、独立更新器进程恢复、失败回滚与升级门禁协调 |
| Rust 后台服务与导出 | `cargo test -p cst-server --locked` | 单实例管道锁、`AppPresence × ChannelMode` 正交门禁与 Cookie 世代作废、10 空闲会话 LRU 与 `unflushed` 快照恢复、安全 `fork`、持久化收件箱、8 MiB 慢连接取消、32 会话向量游标聚合流及 `Chromiumoxide` PNG/HTML 导出契约 |
| 前端静态检查与单测 | `npm run lint --prefix src/web/frontend`、`npm run typecheck --prefix src/web/frontend`、`npm test --prefix src/web/frontend` | 复用 `agent` 目录已安装的 Biome 检查规则，执行路由类型生成、TypeScript 严格检查，以及工具卡片映射、分支树几何、聚合流状态机与乐观发送单测 |
| 前端构建与共享字体 | `npm run build --prefix src/web/frontend`、`npm run test:fonts --prefix src/web/frontend` | 构建 Web 静态资源；校验共享思源黑体可变字体的 SHA-256、OpenType 表和许可证，确认静态目录没有字体副本 |
| Rust Web 浏览器端到端 | `cargo build -p cst-server --bin cst-server --locked`、`npm run test:web:rust-e2e --prefix agent` | 在 Windows 隔离 Chrome 中驱动 `cst-server --standalone-web`，验证乐观发送、33 会话分片聚合流、BitLocker 复查卡片及 PNG/HTML 导出 |
| 设计与文档契约 | `node --test src/test/*.test.mjs` | 画布节点唯一性与颜色令牌子集校验、全仓本地 Markdown 链接与标题锚点校验，以及过渡期工具契约回归 |
| 工程与发行树检查 | `npm run check:engineering --prefix agent`、`npm run check --prefix agent`、`node --test pack/test/launcher.test.mjs pack/test/release-checks.test.mjs` | MJS/JSON/CSS 静态规则、PowerShell PSScriptAnalyzer 1.24.0 检查、启动器与干净发行树红线校验（按 `ci.yml` 初始化 `agent/home/npm` 后可运行全量 `pack/test/*.test.mjs`） |
| 过渡期 TS 专项回归 | `npm run test:oauth --prefix agent`、`npm run test:telemetry --prefix agent`、`npm run test:web --prefix agent`、`npm run test:web:shared-audit --prefix agent`、`npm run test:web:e2e --prefix agent` | 阶段 11 下线旧栈前保留的 TypeScript 认证、遥测、Web 后端与浏览器 E2E 对照测试 |

### CI 执行范围

日常 CI 在 `main` 推送和面向 `main` 的 PR 上运行，同分支新任务自动取消未完成的旧任务。

| 模式 | 检查范围 |
|---|---|
| 日常 | Rust 格式、Clippy（含 `test-support`）与全工作区测试。前端格式、类型、单测与设计文档契约。工程静态检查、过渡期后端类型、OAuth/遥测测试与启动器及干净发行树回归 |
| 完整验收 | 在日常范围上增加前端构建、共享字体校验、`cst-server` Windows 构建与导出测试、过渡期 `test:web` 与 `test:web:shared-audit`、全量 `pack/test/*.test.mjs`、隔离浏览器启停预检，以及 Rust / TS 双入口浏览器 E2E。通过 `gh workflow run ci.yml -f full=true` 手动触发 |

完整验收先单独验证隔离 Chrome 的启动与退出，启动失败时写出 `browser-startup.json` 记录退出码与标准错误，并在超时或失败后回收测试进程。

### Rust 测试环境

Rust CI 使用 `1.99.1` 工具链。日常任务中，Linux 运行工作区非桌面包的静态检查与核心回归，Windows 独立运行 `cst-desktop` 编译、Clippy 与无头界面测试；完整验收在 Windows 同步编译并验证 `cst-server` 与浏览器导出。测试统一将 `CARGO_TARGET_DIR` 与 `PI_CODING_AGENT_DIR` 指向隔离临时目录，并固定设置 `PI_OFFLINE=1`。

`pi-session` 与 `pi-runtime` 的 `test-support` 特性默认关闭，仅在显式启用时编译存储故障注入接口。CI 同时校验默认构建与 `test-support` 构建。

`crates/pi-ai/tests/protocol_contracts.rs` 通过公开 `Models` 接口验证四种对话协议的多工具续答、图片输入与思考签名回放，覆盖事件顺序、超时与取消边界。位于 `crates/pi-ai/tests/stream_events.rs` 的真实 DeepSeek 联调用例默认忽略，仅在显式提供 `PI_AI_LIVE_DEEPSEEK_KEY` 时运行：

```powershell
cargo test -p pi-ai live_deepseek_responses_roundtrip_with_tools --locked -- --ignored --exact
```

LHM 隔离采集辅助程序需在安装 .NET 10 SDK 的构建机上验证，命令如下：

```powershell
./crates/cst-kit/lhm-host/test.ps1 -WorkDirectory E:/tmp/<YYYY-MM-DD>/lhm-tests
```

该脚本构建自包含辅助程序与模拟 DLL，在真实 Windows Job Object 中验证 `gpu` / `sensor` 双模式、非有限值过滤、200 项上限与异常关闭路径，不读取真实硬件。

更新执行与宿主更新测试在 Windows 上运行，使用独立测试密钥与隔离目录编译的模拟主程序验证预写事务替换、15 秒健康检查超时与逆序回滚，不修改当前仓库。认证、BitLocker 与遥测网络测试均连接本机模拟服务（Mock OA、模拟加密卷、本机隔离 TLS 服务），不访问生产端点或宿主加密配置。

## 测试取舍与隔离原则

1. **行为导向**：自动测试验证公开接口、持久化结果、故障恢复与权限边界。禁用源码字符串匹配、内部文件数量断言或脆弱的样式字面量比对。
2. **环境隔离**：测试必须使用临时 `PI_CODING_AGENT_DIR`、隔离浏览器用户目录与本机模拟服务，禁止读取或修改开发者本机真实 `auth.json`、会话记录或生产遥测端点。
3. **诚实记录**：替身测试通过仅证明控制流与协议正确。真实 UAC 提权、真实 BitLocker 加密卷、Windows PE、可移动介质与生产服务联调须单独在实机完成并记录。

## 诊断工具公共核查组（B01–B35）

实机验证时，Agent 仅通过项目工具采集数据，核查者使用独立系统工具或计时对照。`<Win>` 指系统 Windows 目录，`<Kit>` 指工具包目录，`<Sys>` 指系统盘符，`<Vol>` 指工具包所在卷根。

### disk 与 ls（B01–B10）

1. **B01** `disk(space)` 全卷：与可访问卷对照，`freeGB <= totalGB`，剩余空间为 0 时保留数值 `0`。
2. **B02** `disk(info, drive=Kit盘)`：`volumes` 仅含目标卷。物理盘关联成功时按盘过滤，关联失败时返回全量物理盘并附 `infoNotice`。
3. **B03** `disk(health, drive=Sys)`：保留可用 SMART 字段与 `smartErrors`，区分管理员与普通权限的实际错误。
4. **B04** `disk(usage, path=Kit, top=10)`：WizTree 成功时四类排行齐全。降级 `node-walk` 时仅返回目录排行并注明原因。
5. **B05** `disk(usage, path=Win/WinSxS, top=5)` 连续两次：记录冷／热耗时与结果量级。
6. **B06** `disk(usage)` 不存在路径：整次报错，无挂起。
7. **B07** `disk(all)`：`space`、`info`、`health` 独立返回成功数据或各自错误，互不吞没。
8. **B08** `ls(path=Kit/pwsh, top=15)`：按大小倒序，`omitted` 项数与体积准确，未知大小保持 `null`。
9. **B09** `ls(path=Win/WinSxS, top=10)`：在 B05 后命中共享卷缓存 `wiztree-index`，跨卷查询不串用缓存。
10. **B10** `ls(path=Vol)`：卷根目录与文件均可枚举，不可读项与预算停止如实说明。

### sys 与 startup（B11–B16）

1. **B11** `sys(overview)`：CPU 在 `0–100` 之间，`mem.usedMB <= totalMB`，机型与开机时长准确。
2. **B12** `sys(proc, top=15)`：`byCpu` 与 `byMem` 各不超过 15 项，按实际采样间隔计算 CPU，内存对应工作集 `wsMB`。
3. **B13** `sys(gpu)`：适配器与设备管理器一致，单进程 `gpuPct` 取最繁忙单一引擎且不累加超 100，多独显按 `uuid / pciBusId` 区分，NVIDIA 缺失时由 LHM 补充。
4. **B14** `sys(sensor)`：单位准确，允许无可读传感器，`counterErrors` 如实反映热区或频率读取失败，低频不单独判定为过热。
5. **B15** `sys(io)` 连续两次：记录冷／热耗时，明确区分物理盘吞吐与进程读写差分口径。
6. **B16** `startup()`：注册表、启动文件夹、自启服务三类齐全。`disabled` 三态准确，不将 `Run` 的禁用记录套给同名 `RunOnce` 项。

### driver、eventlog 与 runbook（B17–B35）

1. **B17–B22** `driver`：`problem` 区分正常空数组与采集失败。`core` 核对网卡、蓝牙、音频与显示设备及驱动版本。`external` 对照 USB、蓝牙、显示器与可移动盘。`find` 验证 `class`、`id`（含 `HardwareID` 子串与特殊字符）、`name + class` 交集及空条件报错。
2. **B23–B31** `eventlog`：`recent` 时间倒序且未截断时 `sum(counts.n) = total`。`boot`（含 `bluescreen` 去重）、`crash`（含 Information 级 WER 1001 与 `app` 全组过滤）、`service`（排除 7045）以及 `disk`、`query`、`detail`（按 `recordId` 精确读取与轮转未找到）语义准确。`security` 在非管理员下返回 `admin: false, degraded: true`。
3. **B32–B35** `runbook`：在 `artifacts/<当天>/runbook/` 生成 UTF-8 BOM + CRLF 格式 `.txt`。同日序号递增，指定 `sequence` 原地替换并清理旧标题文件，跨天从 `01` 重置。命令含换行或必填项为空时整次报错且不留残余文件，只读介质明确报错。

### BitLocker 实机核查

自动测试通过模拟 COM/WMI 接口覆盖三条隐私边界、隐藏输入、固定目标确认，以及同卷跨进程命名管道锁与强杀重启复查。实机验收先在正常卷执行只读 `status`，再在获准的隔离加密测试卷上核验恢复密码解锁与错误返回码，同步覆盖锁定卷拒绝解密（`0x80310000`）、系统卷自动解锁依赖（`0x80310029`）及后台解密完成判据（`FullyDecrypted`）。

## 跨端共享与 Web 端到端验收

### 跨端共享矩阵（S01–S21）

| 编号 | 核查项 | 通过标准 |
|---|---|---|
| S01–S05 | 通道切换与模型范围 | 软件端与 Web 切换通道后会话状态衔接一致。输入框切换仅改当前会话，模型偏好页保存新会话默认值。各端模型启用范围与排序实时同步 |
| S06–S07 | 三端独立主题与配置合并 | TUI 主题（`settings.json`）、Web 主题（`web-settings.json`）与桌面端设置（`desktop-settings.json`，阶段 9 接入桌面壳层并迁移为 `desktop.json`）互不覆盖。共享设置按字段合并 |
| S08–S12 | 目录、凭据与思考强度 | `models.json` 与 `models-store.json` 更新后即时生效。任一端登录或退出单服务后全局广播同步。思考强度按模型支持档位保存 |
| S13–S15 | 历史、`cwd` 与单写权 | 改名与续答跨端可见。历史会话沿用记录中的 `cwd` 执行 `read` 与 `@` 补全。会话被其他进程持有写权时返回 `409` |
| S16–S21 | 隔离与共享资源 | 忽略工作目录 `AGENTS.md`、保留 `APPEND_SYSTEM.md`。17 工具白名单一致。损坏配置按类别提示且不暴露密钥。收件箱、扫描缓存与遥测互不干扰 |

### Web 端到端核心链路

基于 `bsk` 与 `cst-server --standalone-web` 的完整 Web 验收方案位于 `web` 模块测试目录：

1. [路由广度 E2E（bsk-e2e.md）](../web/test/bsk-e2e.md)：覆盖全部 15 个叶路由与 28 个页内交互分支。
2. [长会话与状态机深度 E2E（bsk-deep-e2e.md）](../web/test/bsk-deep-e2e.md)：验证 500K 上下文自动压缩、瞬态重试、乐观发送对账，以及挂起交互桥与 33 会话聚合流分片。
3. [用户体验量化 E2E（ux-e2e.md）](../web/test/ux-e2e.md)：通过 5 类浏览器注入探针在 6 种视口断点与极限数据夹具下量化检验几何对齐、动效连续性、输入法焦点与流式零抖动。
4. [桌面端 UX 与交互性能 E2E（desktop/test/ux-e2e.md）](../desktop/test/ux-e2e.md)：定义交互性能和体验正确性检查，列出 Win11 上的自动化子集、CPU 与提交内存限流、WARP 降级及补测范围。
5. [Windows 10 / 11 与中国大陆特制版兼容性测试（desktop/test/windows-compat.md）](../desktop/test/windows-compat.md)：列出 Windows 10/11、家庭中文版、神州网信政府版、LTSC 与精简镜像的兼容性风险、Win11 本地验证结果和真机验收清单。
6. [17 个开放工具设计评估与三维度测试规范（diagnostics/test/tools-test-spec.md）](../diagnostics/test/tools-test-spec.md)：按兼容性、性能与字段语义正确性三个维度评估全部 17 个开放工具并定义 `T01–T54` 补充测试矩阵。

CI 自动化浏览器套件覆盖以下核心回归面：

- **会话导出与离线产物**：验证快照时间统一、当前分支与 BitLocker 复查投影、8 任务队列单向状态、同名分页整组换号与 SHA-256 篡改检测。同步验证 Explorer 目录打开、Edge/Chrome 进程复用、1240×3200 两倍图逐行像素连续性、离线 HTML 字体附图内嵌及 10 秒性能上限。
- **外壳边界与页面转场**：验证右侧页面内部独立滚动、侧栏固定铺满窗口、20 条同名会话列表就绪后整组入场，以及滑动选区定位、导出页转场与图片原位缩放预览。
- **流式连续性与过程折叠**：验证 10 会话两批预载、历史折叠懒挂载、流式逐词节点保留，以及特殊组件与普通工具分段计数、代码块与工具原文独立复制。
- **乐观发送与收件箱队列**：验证立即上屏、断线按原 ID 核对、进程重启遗留 `uncertain` 保护，以及运行中立即插话（`steer`）优先于排队项（`queue`）在下一模型调用边界进入。
- **认证、模型偏好与竞态保护**：验证首次缺密钥 24 小时冷却提示、CSTOA 二维码免授权码展示、模型选择器 224px 双级菜单滑动、模型范围勾选排序失败回滚，以及旧弹窗响应不干扰新弹窗等异步竞态边界。

## 实机环境覆盖矩阵

发布前按以下四个维度抽样实机验证，每个维度保留实测记录：

| 维度 | 样本范围 | 重点核查差异项 |
|---|---|---|
| 设备形态（D01–D07） | 组装机、品牌台式机、核显+独显笔记本、一体机、4–7 代酷睿 + 机械盘旧机 | `sys overview` 组装机默认字段、OEM 自启项、双显卡与虚拟显示器枚举、机械盘 I/O 队列与忙碌度 |
| 系统镜像（O01–O08） | Windows 11 家／专、Windows 10（1507–22H2）、政采裁剪镜像、非中文语言包 | 原生 WMI／存储命名空间缺失降级、PDH 英文计数器转本地化路径、事件日志多语言格式化、安全软件虚拟设备与 UKey 枚举 |
| 启动介质（M01–M08） | USB 2.0 / 3.0 U 盘、移动机械盘、PSSD、SD 卡，覆盖 FAT32、exFAT、NTFS | 低速介质启动与 WizTree 导出耗时、非 NTFS 卷标 `wiztree-walk`、换盘拔插后清空旧卷缓存、U 盘原位升级替换 |
| PE 环境（P01–P10） | 微 PE、FirPE、优启通、官方 ADK WinPE，以及只读启动介质 | WMI／事件服务缺失时的结构化报错、`startup` 注明 PE 自身注册表、`wiztree/tmp` 或 `.state/` 不可写时的明确提示 |
