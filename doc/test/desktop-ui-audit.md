# 桌面端视觉、交互与动效核验清单

本文档根据 [cst-pilot-desktop.pen](../../design/cst-pilot-desktop.pen)、[storybook.html](../../design/storybook.html) 与 [DESIGN.md](../../DESIGN.md) 拆解桌面端（`crates/cst-desktop`）待核验清单，覆盖静态设计节点、21 项动效原语、材质层级与跨端交互规则。

## 执行约定

1. **执行流程**：所有核验项按项目技能 [design-visual-audit](../../.agents/skills/design-visual-audit/SKILL.md) 执行隔离启动、双主题截图、双特性门禁与复测闭环；编译缓存、隔离目录、Diff 快照与截图等过程产物统一置于仓库外 `F:/tmp/<YYYY-MM-DD>/` 或 `E:/tmp/<YYYY-MM-DD>/`。
2. **同行评审与直改**：每完成一个大项并确认视觉与门禁通过后，**先在本地执行 `git commit` 留存基准提交**，再新建独立的 `gpt-6.2-sol`（`xhigh` 思考）`pi` 节点（`hc run --bg --name review-<slug> --cwd <repo> pi --provider openai-codex --model gpt-6.2-sol --thinking xhigh`）检查逻辑漏洞、边界异常与工程规范，由其直接修改并单独提交；`sol` 不修改视觉规格、令牌与无障碍 `AutomationId`。收到回复后，对照 `sol` 提交的 `git diff` 核查是否越界并保持合理质疑（不合理改动直接回退或修正），随后重跑门禁验证。
3. **诚实标注**：仅当完成双主题真窗截图确认或代码行为实测通过后才标为 `[x]`；受限于当前能力未覆盖的子项保留 `[ ]` 并附具体差距。
4. **阶段标记提交**：每完成一个阶段性大项（含 `sol` 复核收口与清单更新）后，自动在本地执行一次 `git commit` 作为节点标记（仅本地提交，未获授权不推送远端）。

---

## 一、画布规范与母版基线（`.pen` `LKIQI`、`00A–00F`）

| 区域 / Node ID | 核心核查对象 | 状态 | 进度与遗留说明 |
|---|---|---|---|
| `LKIQI` 设计规范 | 色彩（`AyvAS`）、字号排版（`H8sVa`）、圆角间距高度（`GYpym`）、窗口结构（`Mvvtr`）、材质透明度（`lRw5X`）、汉字 `2px` 光学对齐（`np3Ew`） | `[x]` | 主题色、基础字号、`2/4/8/12px` 与 `np3Ew` 已用于全量界面；`lRw5X` 已实现 `EnableTransparency` 与 `< Feature Level 11_0` 实底回退（`d3d11_single_window_sky_frosted_glass_and_device_recovery` 覆盖 `0xa000` 回退，未篡改宿主注册表 `EnableTransparency=0` 实窗） |
| `00A · 窗口与导航`（`FRLPg`） | `L081M1` 侧栏、`T1U1Xf` 窗控、`GElS8`/`MKlBE` 顶栏、`HUkqs` 对话标题栏、`h5AY3` 多选侧栏、`xWJOf` 子页标题栏、`VPWAF` 设置导航栏 | `[x]` | `L081M1`、`T1U1Xf`、`GElS8`/`MKlBE`、`HUkqs`、`h5AY3`（多选侧栏）、`xWJOf`（子页标题栏）与 `VPWAF`（设置导航栏与 `WzYef` 账户菜单）均已完成双主题 `winapp` 核查 |
| `00B · 按钮与输入`（`Ov79F`） | `ZWAs1` 输入框、`xRNmP` 主钮、`s2CZVU` 描边钮、`U6mIVP` 危险钮、`PP59g` 文字钮、`AxUEZ` 图标钮、`XtKij`/`i3VjFz` 复选框、`ZCGS3` 快捷键、`VngaB`/`gVNCA`/`PHFud` | `[x]` | `ZWAs1`、`ZCGS3`、`gVNCA`、`XtKij`/`i3VjFz`、`U6mIVP`、`PHFud`（服务商/默认模型/筛选下拉）及 1 级红框标红校验态已完成双主题 `winapp` 核查；密文清零与键盘光标导航由 `settings_and_update` 与 `tools_and_bitlocker` 实测 |
| `00C · 菜单与弹窗`（`wbnNW`） | `UWt8w` 右键菜单、`UyJBj` 菜单项、`h1BufF` 顶部通知、`X2gAh` 顶部报错条、`sVucQ` 确认弹窗、`DZ0Fp` 遮罩、`H2p2z` 无遮罩浮层、`K3x72` 弹窗母版 | `[x]` | `UWt8w`/`UyJBj`、`h1BufF`/`X2gAh`、`sVucQ`（退出确认/退出 CSTOA 弹窗）、`DZ0Fp` 与 `H2p2z`/`K3x72` 均已完成双主题 `winapp` 真窗核查，并与 `Esc`/FocusTrap 归还焦点闭环 |
| `00D · 列表与消息`（`psLRr`） | `lzrMn` 会话行、`c4PWw` 信息行、`CpxAu` 用户消息、`b0aK5` 气泡栏、`VS7mw` 助手栏、`BuK2W` 工具状态行 | `[x]` | `lzrMn`、`c4PWw`（设置常规/快捷键/账号信息行）及消息与工具状态行均已完成双主题 `winapp` 核对 |
| `00E · 工具与特殊工具`（`V9ohGI`） | `E9dWo9`/`VM8ko`/`VwO1J` 普通工具三卡、`wmkJw`/`JQMQZ`/`s3WzY`/`ABn8j` 特殊工具四卡 | `[x]` | 七类母版均已完成对齐与双主题验证；`ABn8j` 待决密码输入与 `400` 重试由 `tools_and_bitlocker` 无头集成测试实测，其余终态完成 `winapp` 截图核对 |
| `00F · 背景着色器`（`gcj97`） | `jzHSt` 天空与 `k6JMV` 夜空着色器绑定与静止渲染 | `[x]` | 静态与动态着色器已完成 `WSbcS`/`JN6Rt`/`T932X7` 双主题 `winapp` 验证；切离动态页、失焦/最小化/不可见、`WebActive`、`reduce_motion`、`sys(gpu)`/`sys(sensor)` 及 RDP/软光栅 `0 FPS` 停帧已通过 `shader_frame_scheduler` 与 `d3d11_single_window_sky_frosted_glass_and_device_recovery` 实测 |

---

## 二、业务画板逐屏核查（`.pen` `01–07`）

### 01 · 工作台（`ityMT`）

- [x] `WSbcS` 主页（含 `p2HWHP` 展开态、`OeuCo` 收起态、`PuezS` 深色态、双圆弧标识、无阴影主标题与 `600×88` 输入框；已完成双主题 `winapp` 视觉核查、`h1BufF` 顶部通知定位实测及 `sol` 复核闭环）
- [x] `WSbcS / xdA96` 主页 `X2gAh` 顶部报错条实机触发态截图验证（`X2gAh` 已补充 `12px` 间距与 L2 阴影对齐，并在双主题 `winapp` 主页断连保存失败场景完成触发态真窗截图验证）
- [x] `l84goO` 对话（含 `s0bIf`/`FY9tR` 对话排版、过程折叠头、Markdown 标题/表格/代码块/行内代码深浅色对比、`s4IJwS` 中断与报错详情、`ivhR0` 原比例灯箱、`qFaoI` 深色与本地 SSE 实时流式输出；已完成双主题 `winapp` 真窗核查，并通过 `sol` 工程复核及灯箱冒泡/尺寸/弹窗语义修复与复测）
- [ ] `l84goO / KLzVw & GM9WU` 长会话上滑出现 `40px` 黑底「回到底部」悬浮按钮与 `507` 本机失败用户气泡（`CircleAlert` 图标）真窗触发截图（`KLzVw` 已通过 Win32 滚轮在浅深双主题真窗完成触发态截图与 `scroll-to-bottom-btn` 核验；`GM9WU` 静态与无头集成测试已覆盖，真窗触发态截图待非锁屏桌面补采）
- [x] `zdtJ6` 对话中的工具混排流与多工具折叠切换（已修正独立 `read` 图片脱离折叠段并保持折叠锚点 `expansion_anchor` 不串位，已完成双主题 `winapp` 与 `workbench_and_ime_ui` 单测验证）
- [ ] `Ed7xS` 终端（内置 TUI 视图切换、顶栏分段控件 `topbar-view-gui-btn` / `topbar-view-tui-btn`、窄幅与草稿衔接；阶段 10 待开发：`crates/cst-tui` 尚未创建，点击「终端」切换呈现占位说明「窗口内置终端视图将在阶段 10 开放。」）
- [x] `juqeU` 状态提示与软件更新（`h1BufF` 顶部通知、`WO6NQ` 更新/凭据缺省弹层、`pFooK` 退出确认「继续使用」/「停止任务并退出」与红标已完成双主题 `winapp` 截图核查；更新下载进度、取消与交接锁屏、焦点陷阱与降级草稿检查由 `settings_and_update` 与 `shell_and_mode_ui` 实测覆盖，无线上签名更新包实机截图）
- [ ] `OmK5U` 输入与快捷面板（已对齐输入浮层 Z 轴叠放序、补全标题栏与 `/skill:` 分隔线；`/`/`@` 补全、上下文圆环额度弹层与双级模型/思考强度已完成双主题 `winapp` 截图核查；`queue`/`steer` 乐观重排回滚、IME/动作层键位、防重入与 `sol` 并发修补由 `workbench_and_ime_ui` 实测通过；`X9p2ZR` `Ctrl+P` 命令与导航面板及 `MMTeT` `Ctrl+O` 会话与近期导出面板已完成画布、代码与 `settings_and_update` / `shell_and_mode_ui` 回归测试，待补双主题 `winapp` 真窗核查）
- [ ] `KsR0v` 仪表盘（概览指标卡片、图表/配额布局与深浅色对比度；待开发占位：当前显示「本地用量仪表盘将在后续版本开放。」）

### 02 · 会话管理（`C49doB`）

- [x] `Fp7xb` 全部会话（自然日分组、长标题 `lzrMn` 列表行、`UWt8w`/`UyJBj` 右键菜单、重命名弹窗及空校验 `X2gAh`、`h5AY3` 侧栏多选与全部会话批量模式已完成双主题 `winapp` 截图核查及 `sol` 复核闭环；`409` 删除冲突回滚、失效多选清理与重命名竞态由 `settings_and_update` 与 `shell_and_mode_ui` 实测覆盖）
- [ ] `bliwY` 分支树（五种过滤模式、节点间距与 `2px` 引导线已完成双主题 `winapp` 核查；已按 [ADR-012](../ADR.md) 取消分支树专属快捷键并移除底部提示条，同步调整 `c1wymw` 弹窗为「直接回退 / 总结 / 自定义提示词总结」并去除底部取消按钮，已通过 `settings_and_update` 回归测试，待补双主题 `winapp` 真窗复核）
- [x] `P7uGp` 会话导出（导出选项、格式/主题切换与 `CST Pilot` 预览区已完成双主题 `winapp` 截图核查；生成落盘通知、打开导出目录与离页取消轮询防误开目录由 `settings_and_update` 实测覆盖，真窗未拉起系统资源管理器）

### 03 · 设置（`Trond`）

- [x] `sxkCI` 常规（`VPWAF` 设置导航、外观浅/深/跟随系统切换、系统集成与更新状态行 `c4PWw` 已完成双主题 `winapp` 核查）
- [x] `t7zsNf` 模型服务（已核查列表与 `T5us8O` `已连接` 徽标、自定义网关 `q8NLwK` 增删改、`providers-apikey-input` 掩码及 1 级红框校验、凭据请求流内存清零及 `delete_custom_provider` 修复；真窗 `winapp` 与 `settings_and_update` 闭环）
- [x] `b9mFa` 模型偏好（已核查 `H2p2z` 默认模型/筛选毛玻璃浮层及 Z 轴叠放序、`search`/`refresh-cw`/`plus` 组头、`N 个已启用` 计数、乐观排序回滚及 `cst-server` 全量重排保留顺序；真窗 `winapp` 与 `settings_and_update` 闭环）
- [ ] `i6TeGT` 快捷键（已按 [ADR-012](../ADR.md) 更新为 `Ctrl + P` / `Ctrl + O` 并移除分支树快捷键分组条目，代码与编译已落地，待重跑双主题 `winapp` 真窗核查）

### 04 · 账号与登录（`ABJtA`）

- [x] `JN6Rt` 登录（已核查 `PHFud` 服务商毛玻璃下拉框、`login-apikey-input` 无回显填密、1 级 `$status-danger` 单框校验及输入清除、`Zeroizing` 序列化与 `blue-hour`/`night-sky` 渐变过渡；真窗 `winapp` 与 `settings_and_update` 闭环）
- [x] `T932X7` CSTOA 登录（已核查 `N1Rfg` 二维码状态条、`night-sky`/`blue-sky` 背景着色器双主题真窗表现；离线二维码光栅化、`deviceCode` 解析及轮询代际取消由 `settings_and_update` 实测，离线 `winapp` 夹具无在线扫码服务端）
- [x] `uV4C7` 账号（已通过模拟 `auth.json` 已登录夹具完成双主题 `winapp` 截图核查：`WzYef` 账户菜单、`c4PWw` 信息行、`GOWKl` 操作栏与 `sVucQ` `退出 CSTOA` 弹窗及 `DZ0Fp` 遮罩；实时额度接口由 `settings_and_update` 覆盖）

### 05 · 深色模式部分参考（`wZKdd`）与 06 · 边界情况（`S8xM0`）

- [x] `x8G7D` 深色模式跨组件补检（已完成深色 `sys(overview)`、联网检索 `JQMQZ`、命令清单 `s3WzY` 等卡片在 `1280×720` 真窗下的对比度、徽标与图标着色核查）
- [x] `BqA38` 窗口尺寸与长文本极限边界（已补齐 `bootstrap` 500ms 启动配置加载与跨越 `<1120px` 自动收起/恢复侧栏；已在 `1024×640` 双主题验证 `MKlBE` 与完整 `816px` 对话列、在 `1280×720` 验证 `880px` 标题省略、`runbook` 超长命令与清单文件名截断、宽表格横向滚动、长无空格 URL 与多行报错折行，并通过 `shell_and_mode_ui` 视口断点回归测试）

### 07 · 工具与特殊工具（`uF5B5`）

- [x] `kcHlA` 普通工具全量态（`VwO1J` 原文等宽块/对齐列、`read` 附图独立展开、`FMaqq`/`RvBpL` 降级与报错仅图标着色已完成双主题 `winapp` 核查；17 工具映射与 web 夹具对齐通过单测验证）
- [x] `k6vP8P` 整机概览细节与硬件保护态（已核查 `sys(overview)` 双主题 `详情` 展开列表、`counterErrors` 去重与降级提示；`sys(gpu)`/`sys(sensor)` 停帧由 `shader_frame_scheduler` 集成测试实测）
- [x] `i782jQ` BitLocker 特殊工具卡片（解密进度 `4px` 进度条、`60px` 盘符列、`unknown` 异常中断、已取消与解锁失败态已完成双主题 `winapp` 核查；待决 `ABn8j` 密码框 `bitlocker-pwd-input-*`、`400` 原位报错重试及 IME 隔离由 `tools_and_bitlocker` 无头集成测试实测，静态 `.jsonl` 夹具未覆盖活跃 `requests` 真窗截图）
- [x] `aT4ai` 命令清单卡片（`command_plan`/`runbook_deliver`/失败卡片、`打开` 蓝字外链与底部 `详情` 栏已完成双主题 `winapp` 核查；清单产物写入与打开回调由 `tools_and_bitlocker` 实测）
- [x] `Y5236` 联网检索卡片（三层中性投影 `craft_stack`、降级/失败图标分色与折叠查询列表已完成双主题 `winapp` 核查；悬浮提示条几何按源码 `16px/8px` 与 `4px` 圆角核对）

---

## 三、动效原语与交互行为核查（`design/storybook.html`）

### 21 项动效原语与曲线核查

- [x] **基础按压与悬停组（`press` / `hover` / `focus`）**：`120ms`（`PRESS`）/ `150ms`（`FAST`）与 `ease_out`（`cubic-bezier(0.22, 1, 0.36, 1)`）已对齐并同步系统 `SPI_GETCLIENTAREAANIMATION`；图标按钮接入 `120ms` `Transformation::scale(0.92)`（`animated_icon_press`，支持移出窗口与失焦自动复原），顶栏图标与输入框接入 `150ms` 背景色/聚焦边框与阴影插值（`animated_state_progress`、`lerp_hsla`、`lerp_shadows`）；GPUI `Div` 容器级 `scale(0.97)` 已按 [ADR-008](../ADR.md) 登记等价替代方案
- [x] **浮层与弹窗组（`popover` / `modal` / `toast`）**：已完成 `220ms`/`240ms` `ease_out` 位移淡入（阴影按 `d^3` 渐强）、`2600ms` 顶部通知/报错条 `BannerCountdownState`（`timer_epoch` 防重用 + 悬停暂停/恢复 + 切页/遮挡/替换清悬停），以及最近会话抽屉与账号菜单可折返 `150ms` 无交互退场（关闭首帧即卸交互/无障碍节点并保持 `Esc` 单帧归焦），并经 `shell_and_mode_ui` 与双主题 `winapp` 核实；容器级 `scale(0.96)` 已按 [ADR-008](../ADR.md) 登记替代
- [x] **展开与位移组（`collapse` / `chevron` / `flip` / `pageSlide` / `sidebarShift` / `drawer`）**：过程块与工具原始数据已接入 `MeasuredCollapse` 可折返测量高度插值（`240ms` 开 / `150ms` 关，关闭首帧即卸子交互控件并持续跟踪动态内容高度）；展开侧栏已补齐 `240ms`/`150ms` 双向滑移；侧栏会话、模型偏好与排队栏已接入列表内相对坐标 `FLIP` 位置追踪（不因滚动、筛选或切会话误触发）；`chevron`、`pageSlide`、`sidebarShift` 与 `recent-sessions-drawer` 均经 `shell_and_mode_ui` 与 `sol` 复核实测
- [x] **反馈与指示组（`checkStroke` / `iconSwap` / `segPill` / `tabIndicator`）**：复选勾选已改用 GPUI `canvas` + `PathBuilder::stroke` 实现真矢量两段路径描边动画（`340ms` 勾选 / `150ms` 取消，已通过双主题 `winapp` 核对）；`iconSwap`（`240ms ease_spring` `0.65 -> 1` + `1600ms` 连点重置）、`segPill` 与 `tabIndicator` 均通过无头测试与双主题 `winapp` 实测
- [x] **零延迟与节律组（`tooltipGroup` / `welcomeStagger` / `streamAppend` / `runningLight` / `shaderFreeze`）**：快捷面板 `0ms` 瞬开、欢迎屏单次错位、`RunningLight` 离屏停帧、着色器 `0 FPS` 停帧与减少动态同步均已实测；已实现按窗口隔离的共享 `TooltipOverlay` 冷却组（`theme/tooltip.rs`，`400ms` 初始延迟 / `300ms` 冷却直切，切页/弹窗/失焦/卸载自动清旧提示），挂载于顶栏、侧栏、输入栏、消息操作栏图标按钮并接入 `web-search-warn-tooltip`，已通过 `desktop_tooltips_share_delay_but_not_window_or_anchor_lifetime` 与双主题 `winapp` 核对

---

## 四、材质层级、系统规则与文案红线（`storybook.html` & `DESIGN.md`）

- [x] **L0–L4 五层材质体系**：`L0` 主区与表单采用纯实底；`L1` 仅模态遮罩启用 `DZ0Fp`；`L2` 采用三层中性投影；`L3` `H2p2z`/`K3x72` 毛玻璃渲染、设备重置重建及 `< 11_0` 回退实底已由 `d3d11_single_window_sky_frosted_glass_and_device_recovery` 与双主题 `winapp` 核实（未改写宿主注册表 `EnableTransparency=0` 实窗）；`L4` `inset (0,1px)` 顶光在 `render_frosted_surface` 中排在发丝边与环境投影之前
- [x] **`Esc` 焦点栈与光标规则**：`dismiss_topmost_surface` 与 `sidebar_search_input` 两段式 `Esc`（含组合态 `consume_composing_escape`）已通过 `shell_and_mode_ui` 与 `settings_and_update` 实测并在单帧内归还触发控件焦点；静态扫描确认 `CursorStyle::PointingHand` 仅用于外部网页链接与 `打开` 外部文件触发器，应用内控件统一系统箭头光标
- [x] **输入法与双通道锁定**：多行输入框、搜索框与重命名框 IME 组合隔离、`BitLocker` 密码框 `55` 字节清零与失焦恢复 IME、`WebActive` 全窗锁定遮罩、以及存在运行任务/队列/非空草稿时的退出确认（默认焦点「继续使用」）已通过 `workbench_and_ime_ui`、`tools_and_bitlocker` 与 `shell_and_mode_ui` 全量实测
- [x] **三级异常分流与文案数据规范**：全界面 `1 / 2 / 3A` 分流、3 类标准服务端三句式报错、`crates/cst-desktop/src` 与 `crates/cst-client/src` 零中文感叹号、`YYYY/MM/DD HH:mm` 列表时间、统一 `3.2s` 耗时格式、`tabular-nums` 与数字单位间单半角空格（`MB`/`GB`）均已核查通过（`3B` 态待实机写满信号补截，见 `KLzVw & GM9WU`）
