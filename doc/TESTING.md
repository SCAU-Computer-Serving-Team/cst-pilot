# 测试与验收规范

本文定义项目的自动化检查入口、测试隔离原则、诊断工具核查基准（B01–B35）、跨端共享矩阵与实机环境验收要求。运行日志、截图与临时状态统一写入仓库外当天临时目录（`E:/tmp/<YYYY-MM-DD>/` 或 `F:/tmp/<YYYY-MM-DD>/`）。

## 自动检查与测试入口

以下命令均从仓库根目录执行：

| 范围 | 命令 | 验证职责 |
|---|---|---|
| 工程静态检查 | `npm run check:engineering --prefix agent` | MJS、JSON、CSS 的 Biome 规则与 PowerShell 语法／PSScriptAnalyzer 1.24.0 |
| 后端格式与类型 | `npm run check --prefix agent` | `agent/home/extensions/` 全部扩展的 Biome 检查与严格 TypeScript 类型检查 |
| 前端格式与类型 | `npm run lint --prefix src/web/frontend`、`npm run typecheck --prefix src/web/frontend` | React 规则检查、路由类型生成与 TypeScript 检查 |
| 工具与文档契约 | `node --test src/test/*.test.mjs` | 诊断工具行为、BitLocker 替身与跨进程卷锁、命令方案版本与交付恢复、runbook 原子写入、画布结构与颜色子集、Markdown 本地链接与锚点 |
| 发行脚本与策略 | `node --test pack/test/*.test.mjs` | 启动器权限与参数、TUN 放行策略、离线模型目录、发行树白名单与资源完整性 |
| OAuth 扩展 | `npm run test:oauth --prefix agent` | CSTOA 设备码、轮询、续期、动态模型目录与凭据隔离 |
| Web 后端 | `npm run test:web --prefix agent` | HTTP/SSE 接口、会话池、收件箱队列、鉴权、附件与故障恢复 |
| 跨端共享专项 | `npm run test:web:shared-audit --prefix agent` | 独立 Pi 进程与 Web 共用设置、模型目录、凭据、历史 `cwd`、单写权与独立主题 |
| 前端单元测试 | `npm test --prefix src/web/frontend` | 工具卡片映射、分支树几何算法、会话预载缓存与结束事件合并、草稿图片与通知状态机 |
| 前端构建与字体 | `npm run build --prefix src/web/frontend`、`npm run test:fonts --prefix src/web/frontend` | 静态资源构建、三档 WOFF2 子集覆盖、哈希与许可校验 |
| Web 浏览器端到端 | `npm run test:web:e2e --prefix agent` | 构建后在隔离 Chrome 中验证完整用户链路、持久化与进程重启；模型选择专项覆盖三级菜单、双向左右滑动与同步高度、中途返回、逐级 Esc、外部关闭、保存失败与过期响应、首页传递、键盘、浅深与窄屏 |
| 遥测单元与跨仓库 | `npm run test:telemetry --prefix agent`、`npm run test:telemetry:e2e --prefix agent` | 统计采集、草稿恢复、扣点查询、双端队列、心跳与真实 Go 接收端联调（后者需设置 `CST_TELEMETRY_RECEIVER`） |

## 测试取舍与隔离原则

1. **行为导向**：自动测试验证公开接口、持久化结果、故障恢复与权限边界；禁用源码字符串匹配、内部文件数量或脆弱的 CSS 字面量断言。
2. **环境隔离**：测试必须使用临时 `PI_CODING_AGENT_DIR`、隔离浏览器配置与本机模拟服务（Mock OA / Mock LLM），禁止读取或修改本机真实 `auth.json`、会话记录或生产遥测端点。
3. **诚实记录**：替身测试通过仅证明控制流与协议正确；真实 UAC 弹窗、真实 BitLocker 加密卷、Windows PE 与生产服务联调必须单独在实机环境验证并记录。

## 诊断工具公共核查组（B01–B35）

实机验证时，Agent 仅通过项目工具采集，核查者使用独立 PowerShell、系统工具或计时对照。`<Win>` 指系统 Windows 目录，`<Kit>` 指工具包目录，`<Sys>` 指系统盘符，`<Vol>` 指工具包所在卷根。

### disk 与 ls（B01–B10）

1. **B01** `disk(space)` 全卷：与可访问卷对照，`freeGB <= totalGB`，剩余 0 保留为 `0`。
2. **B02** `disk(info, drive=Kit盘)`：`volumes` 仅目标卷；物理盘关联成功时按盘过滤，关联失败时返回全量物理盘并附 `infoNotice`。
3. **B03** `disk(health, drive=Sys)`：保留可用 SMART 字段与 `smartErrors`；区分管理员与普通权限的实际错误。
4. **B04** `disk(usage, path=Kit, top=10)`：WizTree 成功时四类排行齐全；降级 `node-walk` 时仅含目录排行且标明原因。
5. **B05** `disk(usage, path=Win/WinSxS, top=5)` 连续两次：记录冷／热耗时与结果量级。
6. **B06** `disk(usage)` 不存在路径：整次报错，无挂起。
7. **B07** `disk(all)`：`space`、`info`、`health` 独立返回成功数据或各自错误，互不吞没。
8. **B08** `ls(path=Kit/pwsh, top=15)`：按大小倒序，`omitted` 项数与体积正确，未知大小保持 `null`。
9. **B09** `ls(path=Win/WinSxS, top=10)`：在 B05 后复用共享缓存；跨卷查询不串用缓存。
10. **B10** `ls(path=Vol)`：卷根目录与文件均可枚举，不可读项与预算停止如实说明。

### sys 与 startup（B11–B16）

1. **B11** `sys(overview)`：CPU 在 0–100，`mem.usedMB <= totalMB`，机型与开机时长合理。
2. **B12** `sys(proc, top=15)`：`byCpu` 与 `byMem` 各不超过 15 项，按实际采样间隔计算 CPU，内存对应工作集。
3. **B13** `sys(gpu)`：适配器与设备管理器一致，单进程 `gpuPct` 取最繁忙引擎不累加超 100，多独显按 `uuid/pciBusId` 区分，NVIDIA 缺失可由 LHM 补充。
4. **B14** `sys(sensor)`：单位正确，允许无可读传感器，`counterErrors` 如实反映热区／频率失败，低频不直接判过热。
5. **B15** `sys(io)` 连续两次：记录冷／热耗时，区分物理盘吞吐与进程读写口径。
6. **B16** `startup()`：注册表、启动文件夹、自启服务三类齐全；`disabled` 三态准确，不将 `Run` 状态套给 `RunOnce`。

### driver、eventlog 与 runbook（B17–B35）

1. **B17–B22** `driver`：`problem` 区分正常空数组与采集失败；`core` 核对网卡、蓝牙、音频、显示与四类驱动版本；`external` 对照 USB／蓝牙／显示器／可移动盘；`find` 验证 `class`、`id`（含 `HardwareID` 子串与特殊字符）、`name + class` 交集及空条件报错。
2. **B23–B31** `eventlog`：`recent` 时间倒序且未截断时 `sum(counts.n) = total`；`boot`（含 `bluescreen`）、`crash`（含 Information 级 WER 与 `app` 全分组过滤）、`service`（排除 7045）、`disk`、`query`、`detail`（按 `recordId` 精确读取与轮转未找到）语义正确；`security` 普通权限明确返回 `admin: false, degraded: true`。
3. **B32–B35** `runbook`：在 `artifacts/<当天>/runbook/` 生成 UTF-8 BOM + CRLF 的四行格式 `.txt`；同日序号递增、指定 `sequence` 覆盖修正且清理旧标题文件、跨天从 `01` 重置；命令含换行或必填项为空时整次报错且不留半成品；只读介质明确报错。

### BitLocker 验收

自动测试（`node --test src/test/bitlocker.test.mjs` 与 `bitlocker-lock.test.mjs`）使用 CIM 替身覆盖双端隐藏输入、固定目标确认、同卷跨进程互斥与 JSONL 脱敏。实机验收先在正常卷执行只读 `status`，再在获准的隔离测试加密卷上验证恢复密码解锁、错误密码返回码、锁定卷拒绝解密（`0x80310000`）、自动解锁依赖（`0x80310029`）及后台解密完成判据（`FullyDecrypted`）。

## 跨端共享与 Web 端到端验收

### 跨端共享矩阵（S01–S21）

`npm run test:web:shared-audit --prefix agent` 与 Herdr 实机终端共同验证以下边界：

| 编号 | 核查项 | 通过标准 |
|---|---|---|
| S01–S05 | 会话接管与模型范围 | TUI 落盘后 `/web` 接管一致；TUI `Enter` 与 Web 输入框仅改当前会话，`Ctrl+S` 与 Web 模型偏好保存默认；两端模型范围与顺序双向同步 |
| S06–S07 | 独立主题与配置合并 | TUI 主题（`settings.json`）与 Web 主题（`web-settings.json`）互不覆盖；共享设置按字段合并 |
| S08–S12 | 目录、凭据与思考强度 | `models.json` 与 `models-store.json` 更新后重读生效；双端登录与单服务退出同步；思考强度按模型档位保存 |
| S13–S15 | 历史、`cwd` 与单写权 | 改名与续答双向可见；历史会话沿用原记录 `cwd` 执行 `read` 与 `@` 补全；一端持有写权时另一端拒绝写入（409） |
| S16–S21 | 隔离与共享资源 | 禁用 `AGENTS.md`、保留 `APPEND_SYSTEM.md`；开放工具白名单一致；损坏配置提示类别且不泄密；收件箱、扫描缓存与遥测互不干扰 |

`web-reopen.test.ts` 验证待机终端可重复输入 `/web` 打开浏览器、复用原服务，以及其他命令和退出快捷键不分发。真实 TUI 另核查关闭浏览器标签页或窗口后重新打开，确认原会话与后台任务保留。

`runtime-input.test.ts` 验证本地扩展命令不刷新模型运行时、不解析可用范围、不检查凭据，以及部分或全部范围失效后的刷新与 Pi 原生发送错误。真实 TUI 另核查 `/model`、`/login` 与 `/web`，使用无凭据、失效范围的隔离配置。

### Web 端到端核心链路

`npm run test:web:e2e --prefix agent` 覆盖以下完整浏览器链路：

- **会话导出与产物**：`session-export.test.ts` 验证快照统一取时、跨天日期、当前分支、过程筛选、脱敏、目录越界、队列、失败回收、重启登记与文件修改检测；`export-controls.e2e.ts` 覆盖浅深窄屏、下拉箭头几何对齐、菜单逐帧动效与键盘、实时分支及离页任务；`session-export.e2e.ts` 通过真实 API 和页面生成浅深 PNG、长文分图与离线 HTML，断网打开 HTML 并核对字体、附图、代码和折叠，保存实际文件及预览截图。`export-performance.e2e.ts` 通过真实 API 测量短会话与 500 段含代码长会话的 PNG / HTML 从请求到保存完成，逐项断言小于 10 秒，并核对高清尺寸、内容和下载完整性；`export-render.e2e.ts` 对分图逐行核对色带像素，检查空白、重复、遗漏与尾页裁切。Runbook 契约覆盖指定日期、方案恢复和 `artifacts/<日期>/runbook/`。
- **命令多轮与布局**：`runbook-roundtrip.e2e.ts` 使用实际方案存储生成两轮修订，覆盖重复调用 ID、结果缺少 `details`、复制、文件打开、压缩后刷新和服务重启恢复；在 1920、1366、960、390px 窗口覆盖正文与输入框边缘对齐、内容列宽度上限及浅深主题。
- **预载与流式连续性**：`session-preload.e2e.ts` 验证七天内最新十项自动预载、点击前路由入口就绪、首次及重复切换无需补载 JS/CSS、延迟刷新下同步切页、无旧正文、浅深与窄屏扫光及减少动态；`session-navigation.e2e.ts` 验证同名会话连续滑动、单层按下反馈、尺寸变化不中断滑动、正文淡入、历史 Markdown 节点数量、取消点击、修饰键、键盘导航、快速切换、历史返回与读取失败恢复；`process-disclosure.e2e.ts` 验证历史过程首次展开才挂载、首次展开高度动画、收起再展开保留节点及分页状态；`streaming-after-tool.e2e.ts` 验证运行中过程立即挂载、完成后保留节点、逐段正文及逐词节点在定稿后保留，延迟快照下正文不闪空。
- **代码块复制**：`code-copy.e2e.ts` 覆盖高亮、无语言和缩进代码块、工具原文、换行保留、独立反馈、键盘、失败重试及浅深窄屏；`markdown-rendering.e2e.ts` 验证流式与定稿复制，`notification-lifecycle.e2e.ts` 验证技术详情复制与 Esc 焦点恢复。剪贴板测试使用浏览器内替身，不改动用户剪贴板。
- **圆角与表格**：`radius.e2e.ts` 覆盖 1440、768、390px 浅深主题，检查 Markdown 表格圆角裁切、单层描边、键盘横向滚动、圆角令牌、特殊工具内层控件、分段控件内缩、全圆例外、登录间距网格与无超椭圆增强时的基础样式。
- **转场清晰度**：`motion-clarity.e2e.ts` 检查已加载动效关键帧与实际页面快照均无高斯模糊，登录页静态背景保留模糊。
- **图片预览**：`image-preview.e2e.ts` 覆盖消息、工具与草稿的共用预览，原位缩放与中途关闭、键盘打开与 Esc 收回、焦点与滚动恢复、窗口变化、浅深主题、窄屏和减少动态。
- **特殊工具过程计数**：`turn-segments.test.ts` 与 `tool-state.test.ts` 覆盖所有特殊工具分类、普通工具进度、图片和空段；`special-tool-process.e2e.ts` 验证特殊组件后思考／正文不出现零次计数，保留内容、流式手动收起与工作时长入口，并核对刷新和浅深／窄屏。
- **消息、流式与工具**：首页发送、网络响应丢失后保留草稿并幂等重试、单工具独立计时与冻结、过程折叠、特殊工具组件（BitLocker、整机概览、runbook、联网检索）交互、Markdown 块间距与代码高亮。
- **会话管理与分支**：侧栏最近二十项与 16px 字号、折叠、全部会话页四组时间边界与浅深设计稿、原生复选框与范围内全选、乐观删除与失败回滚、`/fork` 派生选择、`/tree` 五档过滤与几何对齐、分支总结三选项与生成中取消。
- **首次模型配置**：`manual-model-selection.test.ts` 与同名端到端测试验证无密钥时保留默认身份、其他服务不替代、切页与刷新不重复自动提示、主动发送仍弹窗、草稿保留、配置后返回、默认值与会话选择分离，以及浅深和窄屏状态。
- **模型热更新**：`model-catalog.test.ts` 验证 `PI_OFFLINE=1` 下刷新、缓存、并发、取消、临时密钥不落盘与登录刷新去重；`model-catalog.e2e.ts` 验证手动按钮、请求去重、后台 SSE 自动同步、失败缓存及浅深／窄屏布局。
- **CSTOA 扫码展示**：`cstoa-device-login.e2e.ts` 验证登录页与服务配置页不显示设备授权码，保留真实二维码、倒计时、刷新、取消和手机授权后回账号页；覆盖浅深与窄屏，并验证其他 OAuth 服务仍展示必要设备码。
- **设置、登录与退出**：`settings-tabs.e2e.ts` 验证四分类双向滑动、下划线移动、连续切换、键盘与历史导航、浅深及窄屏、减少动态与无快照降级；另覆盖四分类设置路由、自定义模型服务增删改、模型范围拖拽与键盘排序、CSTOA 扫码与 APIKEY 登录、上下文额度面板、全局顶部通知、断线恢复、空闲与运行中安全退出确认。

## 实机环境覆盖矩阵

发布前按以下四个维度抽样实机验证，不要求穷举笛卡尔积，但每个维度须有实测记录：

| 维度 | 样本范围 | 重点核查差异项 |
|---|---|---|
| 设备形态（D01–D07） | 组装机、品牌台式机、核显+独显笔记本、一体机、4–7 代酷睿 + 机械盘旧机 | `sys overview` 组装机默认字段、OEM 自启项、双显卡与虚拟显示器枚举、机械盘 IO 队列与忙碌度 |
| 系统镜像（O01–O08） | Windows 11 家／专、Windows 10（1507–22H2）、政采裁剪镜像、非中文语言包 | 随包 `pwsh` 启动、CIM／存储模块缺失报错、PDH 计数器本地化路径、事件日志语言渲染、安全软件虚拟设备与 UKey 枚举 |
| 启动介质（M01–M08） | USB 2.0 / 3.0 U 盘、移动机械盘、PSSD、SD 卡；覆盖 FAT32、exFAT、NTFS | 低速介质 `pi.cmd` 启动与 WizTree 导出耗时、非 NTFS 卷标 `wiztree-walk`、大文件统计、拔插换盘后清除旧卷缓存 |
| PE 环境（P01–P10） | 微 PE、FirPE、优启通、官方 ADK WinPE；只读启动介质 | `prx.bin` 与 `pwsh` 启动、WMI／事件服务缺失時の结构化降级、`startup` 标明 PE 自身配置、`wiztree/tmp` 不可写时的降级与 `.state` 写入边界 |
