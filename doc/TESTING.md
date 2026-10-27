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
| Web 浏览器端到端 | `npm run test:web:e2e --prefix agent` | 构建后在隔离 Chrome 中验证完整用户链路、持久化与进程重启 |
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
3. **B32–B35** `runbook`：在 `outbox/<当天>/` 生成 UTF-8 BOM + CRLF 的四行格式 `.txt`；同日序号递增、指定 `sequence` 覆盖修正且清理旧标题文件、跨天从 `01` 重置；命令含换行或必填项为空时整次报错且不留半成品；只读介质明确报错。

### BitLocker 验收

自动测试（`node --test src/test/bitlocker.test.mjs` 与 `bitlocker-lock.test.mjs`）使用 CIM 替身覆盖双端隐藏输入、固定目标确认、同卷跨进程互斥与 JSONL 脱敏。实机验收先在正常卷执行只读 `status`，再在获准的隔离测试加密卷上验证恢复密码解锁、错误密码返回码、锁定卷拒绝解密（`0x80310000`）、自动解锁依赖（`0x80310029`）及后台解密完成判据（`FullyDecrypted`）。

## 跨端共享与 Web 端到端验收

### 跨端共享矩阵（S01–S21）

`npm run test:web:shared-audit --prefix agent` 与 Herdr 实机终端共同验证以下边界：

| 编号 | 核查项 | 通过标准 |
|---|---|---|
| S01–S05 | 会话接管与模型范围 | TUI 落盘后 `/web` 接管一致；TUI `Enter` 仅改当前会话、`Ctrl+S` 与 Web 选择保存默认；两端模型范围与顺序双向同步 |
| S06–S07 | 独立主题与配置合并 | TUI 主题（`settings.json`）与 Web 主题（`web-settings.json`）互不覆盖；共享设置按字段合并 |
| S08–S12 | 目录、凭据与思考强度 | `models.json` 与 `models-store.json` 更新后重读生效；双端登录与单服务退出同步；思考强度按模型档位保存 |
| S13–S15 | 历史、`cwd` 与单写权 | 改名与续答双向可见；历史会话沿用原记录 `cwd` 执行 `read` 与 `@` 补全；一端持有写权时另一端拒绝写入（409） |
| S16–S21 | 隔离与共享资源 | 禁用 `AGENTS.md`、保留 `APPEND_SYSTEM.md`；开放工具白名单一致；损坏配置提示类别且不泄密；收件箱、扫描缓存与遥测互不干扰 |

### Web 端到端核心链路

`npm run test:web:e2e --prefix agent` 覆盖以下完整浏览器链路：

- **命令多轮与布局**：`runbook-roundtrip.e2e.ts` 使用实际方案存储生成两轮修订，覆盖重复调用 ID、结果缺少 `details`、复制、文件打开、压缩后刷新和服务重启恢复；在 1920、1366、960、390px 窗口覆盖正文与输入框边缘对齐、内容列宽度上限及浅深主题。
- **预载与流式连续性**：`session-preload.e2e.ts` 验证七天内最新十项自动预载，延迟刷新下同步切页、无旧正文、浅深与窄屏扫光及减少动态；`streaming-after-tool.e2e.ts` 验证追加段落与定稿后已读节点保留，延迟快照下正文不闪空。
- **消息、流式与工具**：首页发送、网络响应丢失后保留草稿并幂等重试、单工具独立计时与冻结、过程折叠、特殊工具组件（BitLocker、整机概览、runbook、联网检索）交互、Markdown 块间距与代码高亮。
- **会话管理与分支**：侧栏日期分组、多选与整组勾选、乐观删除与失败回滚、`/fork` 派生选择、`/tree` 五档过滤与几何对齐、分支总结三选项与生成中取消。
- **设置、登录与退出**：四分类设置路由、自定义模型服务增删改、模型范围拖拽与键盘排序、CSTOA 扫码与 APIKEY 登录、上下文额度面板、全局顶部通知、断线恢复、空闲与运行中安全退出确认。

## 实机环境覆盖矩阵

发布前按以下四个维度抽样实机验证，不要求穷举笛卡尔积，但每个维度须有实测记录：

| 维度 | 样本范围 | 重点核查差异项 |
|---|---|---|
| 设备形态（D01–D07） | 组装机、品牌台式机、核显+独显笔记本、一体机、4–7 代酷睿 + 机械盘旧机 | `sys overview` 组装机默认字段、OEM 自启项、双显卡与虚拟显示器枚举、机械盘 IO 队列与忙碌度 |
| 系统镜像（O01–O08） | Windows 11 家／专、Windows 10（1507–22H2）、政采裁剪镜像、非中文语言包 | 随包 `pwsh` 启动、CIM／存储模块缺失报错、PDH 计数器本地化路径、事件日志语言渲染、安全软件虚拟设备与 UKey 枚举 |
| 启动介质（M01–M08） | USB 2.0 / 3.0 U 盘、移动机械盘、PSSD、SD 卡；覆盖 FAT32、exFAT、NTFS | 低速介质 `pi.cmd` 启动与 WizTree 导出耗时、非 NTFS 卷标 `wiztree-walk`、大文件统计、拔插换盘后清除旧卷缓存 |
| PE 环境（P01–P10） | 微 PE、FirPE、优启通、官方 ADK WinPE；只读启动介质 | `prx.bin` 与 `pwsh` 启动、WMI／事件服务缺失時の结构化降级、`startup` 标明 PE 自身配置、`wiztree/tmp` 不可写时的降级与 `.state` 写入边界 |
