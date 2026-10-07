# 共享后端跨端联调报告

2026-10-07。本轮共享边界、独立主题与动效修复已通过本机回归。真实 Herdr TUI/Web 联调15组通过，另有9组待机输入故障回归；完整 MVP、生产授权与正式发行验收仍未完成。

计划见[专项矩阵](shared-backend.md)。本页只记录当前有效结果，规格与实现分别见[会话运行](../web/SPEC/session-runtime.md)、[模型配置](../web/SPEC/auth.md)和[运行设计](../design/web/runtime.md)。

## 环境

| 项目 | 范围 |
|---|---|
| 源码 | preview/prev0.5，1505e90之后的本轮修复；最终提交以Git与PR为准 |
| 官方运行 | Pi0.85.1，内置Bun1.3.14 / Node兼容24.3.0 |
| 发行副本 | 完整prev0.5副本更新本轮扩展与预构建页面，保留全部packages；此副本用于测试，不是新的交付ZIP |
| TUI | Windows11、PowerShell7.6.6，Herdr隔离邻居；两个项目共用同一实验home |
| 浏览器 | Chrome154独立profile；本轮使用CDP浏览器操作，不代表系统IME或原生粘贴拖拽 |
| 自动化 | 仓库Node22.23.2；7项独立Pi进程验收和常规浏览器E2E |
| 外部边界 | 模型/OA HTTP模拟；Pi、文件、工具、会话、服务和进程真实。授权只使用实验占位值，遥测关闭 |

## 真实双端结果

| 组 | 操作 | 实际结果 |
|---|---|---|
| F01 | TUI保存、high思考、/web接管与两类Web续答 | 同ID与模型two保留，AGENTS均未进入请求，APPEND_SYSTEM保留，两端工具均14个 |
| F02 | TUI持有B时Web提交 | 409 `session_owned`，原JSONL不变，无额外模型请求；TUI仍可续答 |
| F03 | B的TUI退出后Web重开 | 最新TUI消息可见，实际相对read为`PROJECT_B_ONLY` |
| F04 | Web持有时另一TUI启动同历史 | 明确拒绝；另建独立TUI会话正常可用 |
| F05 | Web保存范围，运行TUI重进菜单并循环三次 | 菜单2/4，实际请求只调用two/three |
| F06 | TUI范围菜单保存单模型 | Web读取相同three范围 |
| F07 | 更新models.json端点 | 两端后续实际请求都使用新端点，无需重启 |
| F08 | Web换B密钥、TUI登录A、单服务退出 | 实际授权依次匹配B/A；退出后无模型请求，other凭据保留 |
| F09 | 两端disk技能 | 同文件测试标记在实际展开请求中存在 |
| F10 | 两方向自然页面切换 | ready→finished完整结束；进入smooth-out，退出smooth-in，实际分段曲线正确 |
| F11 | Web改名、保存low并退出，再由TUI重开 | 新名称、Web消息与low思考状态恢复 |
| F12 | TUI/reload再继续、重进范围、另一端尝试打开 | 继续可用，写权保持，重读仍有效，无重复绑定 |
| F13 | 实际TUI和Web分别选择主题 | TUI light与Web dark并存；Web写外观不修改Pi配置文件 |
| F14 | models.json损坏与修正 | 503 `configuration_unavailable`，说明文件类别；恢复文件后200，无需重启 |
| F15 | 两项目特有文件与同名相对read | Web读取B，文件接口仅有B-only而无A-only |

最终有效证据为`interop/run4.log`、`final-results.json`和`extra-results.json`。准备阶段的菜单未就绪、输入匹配错误和脚本失败另存原始日志，没有计入上述通过项。

## 自动回归

| 范围 | 判定 |
|---|---|
| 独立进程共享验收 | 7项：默认模型、上下文、历史cwd及文件清单、跨进程拒绝与释放、独立主题、损坏配置恢复、运行中范围 |
| 写权与分支回归 | 同身份拒绝、不同身份独立、释放可重用；创建分支保持源ID、cwd和写权 |
| 跨项目文件补全 | 实际浏览器A→B→A切换，清单和前端缓存均与原会话cwd一致 |
| 页面动效 | 每个快照读取实际keyframes easing；浅深、窄屏、减少动态、加载替换与自然完成保留 |
| 常规E2E | 分支总结、导航、编辑器、首轮时长、共享配置、工具计时、树对照、错误与退出等链路 |

自动命令与原始日志保存到本机证据目录。测试等待本次用户消息对应的最终回复，避免把前一轮已完成回复当作当前轮次完成。

本机最终检查：131项前端、68项后端、11项OAuth、7项共享验收、13项设计、9项发行脚本、4项字体与11项浏览器E2E通过。格式、类型、99份文档链接/锚点及过时结论核查通过。

待机故障回归在完整官方副本中注入损坏settings/models，再输入文字、内置命令和快捷键9组；没有读取损坏配置或分发命令，历史未改，HTTP可用。正常退出后测试进程与邻居关闭。

## 动效判定

| 方向 | 主时长 | 曲线与错位 |
|---|---|---|
| 主页→工作区 | 背景退出500ms | 退出`cubic-bezier(0.64, 0, 0.78, 0)`；工作区进入smooth-out |
| 工作区→主页 | 背景及主内容进入700ms | 进入`cubic-bezier(0.22, 1, 0.36, 1)`；输入框延迟56ms、页脚112ms，总812ms；工作区退出smooth-in |

曲线互为时间反向，时长和错位分别定义。自然播放耗时还包含帧调度，不等同单个CSS时长；本轮不声明整条页面切换按同一时间轴反放。

## 未验证范围

- models-store.json独立编辑后的全目录行为，全部技能、提示词模板与工具资源组合。
- 系统IME、系统剪贴板图片、Explorer拖拽、真实低端GPU和高刷屏。
- Windows10、最低Chromium111、普通权限和多用户、跨机/U盘与杀毒软件。
- 生产OA/供应商授权、真实网络延迟、全部多标签队列和附件组合。
- 第三方脚本直接写JSONL或未加载本版本运行协调扩展的外部Pi；协议不约束这些写入者。

## 证据与清理

本机根目录：`E:/tmp/2026-10-07/shared-fixes/`。

| 路径 | 内容 |
|---|---|
| `interop/final-results.json`、`run4.log` | 11组实际TUI/Web联调 |
| `interop/extra-results.json`、`extra.log` | 重载、独立主题、配置恢复、目录及补全4组 |
| `interop/model-requests.json` | 实际模型、端点、工具、上下文与测试授权匹配，无真实用户密钥 |
| `interop/final-motion.json` | 自然ready/finished、分段曲线、时长与错位 |
| `interop/*-tui.txt`、`*.png` | 实际TUI读屏、主题、接管和正确cwd窗口 |
| `parked-result.json`、`parked-tui.txt` | 损坏配置下9组待机输入回归 |
| `pack-verify.log`、`pack-verify/_smoke/` | 干净装配、官方exe真实read续答/退出、ZIP解压与清单验证 |
| `last-checks.json`、`final-*.log`、`documentation-audit.json` | 最终自动回归和全文文档核查 |
| `audit*.log`、`owner-tests-fixed.log` | 公开接口和进程/文件断言 |

测试TUI均正常退出，只关闭自行创建的Herdr邻居；模拟模型服务按PID与命令路径确认后停止。实验home、凭据、会话与profile未入库，用户原有画布改动保留。
