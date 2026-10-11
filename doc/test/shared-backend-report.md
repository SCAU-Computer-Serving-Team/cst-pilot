# 共享后端跨端联调报告

2026-10-07。本轮共享边界、独立主题与动效修复已通过本机回归。真实 Herdr TUI/Web 联调 15 组通过，另有 9 组待机输入故障回归；完整 MVP、生产授权与正式发行验收仍未完成。

计划见[专项矩阵](shared-backend.md)。本页只记录当前有效结果，规格与实现分别见[会话运行](../web/SPEC/session-runtime.md)、[模型配置](../web/SPEC/auth.md)和[运行设计](../design/web/runtime.md)。

## 环境

| 项目 | 范围 |
|---|---|
| 源码 | preview/prev0.5，1505e90 之后的本轮修复；最终提交以 Git 与 PR 为准 |
| 官方运行 | Pi 0.85.1，内置 Bun 1.3.14 / Node 兼容 24.3.0 |
| 发行副本 | 完整 prev0.5 副本更新本轮扩展与预构建页面，保留全部 packages；此副本用于测试，不是新的交付 ZIP |
| TUI | Windows 11、PowerShell 7.6.6，Herdr 隔离邻居；两个项目共用同一实验 home |
| 浏览器 | Chrome 154 独立 profile；本轮使用 CDP 浏览器操作，不代表系统 IME 或原生粘贴拖拽 |
| 自动化 | 仓库 Node 22.23.2；7 项独立 Pi 进程验收和常规浏览器 E2E |
| 外部边界 | 模型/OA HTTP 模拟；Pi、文件、工具、会话、服务和进程真实。授权只使用实验占位值，遥测关闭 |

## 真实双端结果

| 组 | 操作 | 实际结果 |
|---|---|---|
| F01 | TUI 保存、high 思考、/web 接管与两类 Web 续答 | 同 ID 与模型 two 保留，AGENTS 均未进入请求，APPEND_SYSTEM 保留，两端工具均 14 个 |
| F02 | TUI 持有 B 时 Web 提交 | 409 `session_owned`，原 JSONL 不变，无额外模型请求；TUI 仍可续答 |
| F03 | B 的 TUI 退出后 Web 重开 | 最新 TUI 消息可见，实际相对 read 为 `PROJECT_B_ONLY` |
| F04 | Web 持有时另一 TUI 启动同历史 | 明确拒绝；另建独立 TUI 会话正常可用 |
| F05 | Web 保存范围，运行 TUI 重进菜单并循环三次 | 菜单 2/4，实际请求只调用 two/three |
| F06 | TUI 范围菜单保存单模型 | Web 读取相同 three 范围 |
| F07 | 更新 models.json 端点 | 两端后续实际请求都使用新端点，无需重启 |
| F08 | Web 换 B 密钥、TUI 登录 A、单服务退出 | 实际授权依次匹配 B/A；退出后无模型请求，other 凭据保留 |
| F09 | 两端 disk 技能 | 同文件测试标记在实际展开请求中存在 |
| F10 | 两方向自然页面切换 | ready→finished 完整结束；分段曲线与当次配置一致，当前衔接另经序列回归 |
| F11 | Web 改名、保存 low 并退出，再由 TUI 重开 | 新名称、Web 消息与 low 思考状态恢复 |
| F12 | TUI/reload 再继续、重进范围、另一端尝试打开 | 继续可用，写权保持，重读仍有效，无重复绑定 |
| F13 | 实际 TUI 和 Web 分别选择主题 | TUI light 与 Web dark 并存；Web 写外观不修改 Pi 配置文件 |
| F14 | models.json 损坏与修正 | 503 `configuration_unavailable`，说明文件类别；恢复文件后 200，无需重启 |
| F15 | 两项目特有文件与同名相对 read | Web 读取 B，文件接口仅有 B-only 而无 A-only |

最终有效证据为 `interop/run4.log`、`final-results.json` 和 `extra-results.json`。准备阶段的菜单未就绪、输入匹配错误和脚本失败另存原始日志，没有计入上述通过项。

## 自动回归

| 范围 | 判定 |
|---|---|
| 独立进程共享验收 | 7 项：默认模型、上下文、历史 cwd 及文件清单、跨进程拒绝与释放、独立主题、损坏配置恢复、运行中范围 |
| 写权与分支回归 | 同身份拒绝、不同身份独立、释放可重用；创建分支保持源 ID、cwd 和写权 |
| 跨项目文件补全 | 实际浏览器 A→B→A 切换，清单和前端缓存均与原会话 cwd 一致 |
| 页面动效 | 每个快照读取实际 keyframes easing；浅深、窄屏、减少动态、加载替换与自然完成保留 |
| 常规 E2E | 分支总结、导航、编辑器、首轮时长、共享配置、工具计时、树对照、错误与退出等链路 |

自动命令与原始日志保存到本机证据目录。测试等待本次用户消息对应的最终回复，避免把前一轮已完成回复当作当前轮次完成。

当前源码提交检查：132 项前端、68 项后端、11 项 OAuth、7 项共享验收、13 项设计、10 项发行脚本、4 项字体与 13 项顺序浏览器 E2E 通过，格式和类型检查通过。共享修复时的 99 份文档链接/锚点核查记录保留在证据目录。

待机故障回归在完整官方副本中注入损坏 settings/models，再输入文字、内置命令和快捷键 9 组；没有读取损坏配置或分发命令，历史未改，HTTP 可用。正常退出后测试进程与邻居关闭。

## 动效判定

| 方向 | 主时长 | 曲线与错位 |
|---|---|---|
| 主页→工作区 | 总 400ms | 背景实色 ease-in-out 退出 250ms；工作区延迟 150ms 后 smooth-out 进入 250ms，重叠 100ms |
| 工作区→主页 | 总 400ms | 工作区 smooth-out 退出 150ms；背景和主页内容延迟 150ms 后 smooth-out 进入 250ms |

当前动效经独立序列测试和页面专项核查。共享后端窗口联调记录只证明当次切换完整结束，最新时间与曲线见[前端设计](../design/web/frontend.md)。自然播放耗时包含帧调度，不等同 CSS 总时长。

## 未验证范围

- models-store.json 独立编辑后的全目录行为，全部技能、提示词模板与工具资源组合。
- 系统 IME、系统剪贴板图片、Explorer 拖拽、真实低端 GPU 和高刷屏。
- Windows 10、最低 Chromium 111、普通权限和多用户、跨机/U 盘与杀毒软件。
- 生产 OA/供应商授权、真实网络延迟、全部多标签队列和附件组合。
- 第三方脚本直接写 JSONL 或未加载本版本运行协调扩展的外部 Pi；协议不约束这些写入者。

## 证据与清理

本机根目录：`E:/tmp/2026-10-07/shared-fixes/`。

| 路径 | 内容 |
|---|---|
| `interop/final-results.json`、`run4.log` | 11 组实际 TUI/Web 联调 |
| `interop/extra-results.json`、`extra.log` | 重载、独立主题、配置恢复、目录及补全 4 组 |
| `interop/model-requests.json` | 实际模型、端点、工具、上下文与测试授权匹配，无真实用户密钥 |
| `interop/final-motion.json` | 自然 ready/finished、分段曲线、时长与错位 |
| `interop/*-tui.txt`、`*.png` | 实际 TUI 读屏、主题、接管和正确 cwd 窗口 |
| `parked-result.json`、`parked-tui.txt` | 损坏配置下 9 组待机输入回归 |
| `pack-verify.log`、`pack-verify/_smoke/` | 干净装配、官方 exe 真实 read 续答/退出、ZIP 解压与清单验证 |
| `last-checks.json`、`final-*.log`、`documentation-audit.json` | 最终自动回归和全文文档核查 |
| `audit*.log`、`owner-tests-fixed.log` | 公开接口和进程/文件断言 |

测试 TUI 均正常退出，只关闭自行创建的 Herdr 邻居；模拟模型服务按 PID 与命令路径确认后停止。实验 home、凭据、会话与 profile 未入库，用户原有画布改动保留。
