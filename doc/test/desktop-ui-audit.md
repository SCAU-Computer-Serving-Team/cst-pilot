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
| `LKIQI` 设计规范 | 色彩（`AyvAS`）、字号排版（`H8sVa`）、圆角间距高度（`GYpym`）、窗口结构（`Mvvtr`）、材质透明度（`lRw5X`）、汉字 `2px` 光学对齐（`np3Ew`） | `[ ]` | 主题色、基础字号、`2/4/8/12px` 与 `np3Ew` 已用于主体页；`lRw5X` 降透明回退待随 `00C` 实测 |
| `00A · 窗口与导航`（`FRLPg`） | `L081M1` 侧栏、`T1U1Xf` 窗控、`GElS8`/`MKlBE` 顶栏、`HUkqs` 对话标题栏、`h5AY3` 多选侧栏、`xWJOf` 子页标题栏、`VPWAF` 设置导航栏 | `[x]` | `L081M1`、`T1U1Xf`、`GElS8`/`MKlBE`、`HUkqs`、`h5AY3`（多选侧栏）、`xWJOf`（子页标题栏）与 `VPWAF`（设置导航栏与 `WzYef` 账户菜单）均已完成双主题 `winapp` 核查 |
| `00B · 按钮与输入`（`Ov79F`） | `ZWAs1` 输入框、`xRNmP` 主钮、`s2CZVU` 描边钮、`U6mIVP` 危险钮、`PP59g` 文字钮、`AxUEZ` 图标钮、`XtKij`/`i3VjFz` 复选框、`ZCGS3` 快捷键、`VngaB`/`gVNCA`/`PHFud` | `[x]` | `ZWAs1`、`ZCGS3`、`gVNCA`、`XtKij`/`i3VjFz`、`U6mIVP`、`PHFud`（服务商/默认模型/筛选下拉）及 1 级红框标红校验态已完成双主题 `winapp` 核查；密文清零与键盘光标导航由 `settings_and_update` 与 `tools_and_bitlocker` 实测 |
| `00C · 菜单与弹窗`（`wbnNW`） | `UWt8w` 右键菜单、`UyJBj` 菜单项、`h1BufF` 顶部通知、`X2gAh` 顶部报错条、`sVucQ` 确认弹窗、`DZ0Fp` 遮罩、`H2p2z` 无遮罩浮层、`K3x72` 弹窗母版 | `[x]` | `UWt8w`/`UyJBj`、`h1BufF`/`X2gAh`、`sVucQ`（退出确认/退出 CSTOA 弹窗）、`DZ0Fp` 与 `H2p2z`/`K3x72` 均已完成双主题 `winapp` 真窗核查，并与 `Esc`/FocusTrap 归还焦点闭环 |
| `00D · 列表与消息`（`psLRr`） | `lzrMn` 会话行、`c4PWw` 信息行、`CpxAu` 用户消息、`b0aK5` 气泡栏、`VS7mw` 助手栏、`BuK2W` 工具状态行 | `[x]` | `lzrMn`、`c4PWw`（设置常规/快捷键/账号信息行）及消息与工具状态行均已完成双主题 `winapp` 核对 |
| `00E · 工具与特殊工具`（`V9ohGI`） | `E9dWo9`/`VM8ko`/`VwO1J` 普通工具三卡、`wmkJw`/`JQMQZ`/`s3WzY`/`ABn8j` 特殊工具四卡 | `[x]` | 七类母版均已完成对齐与双主题验证；`ABn8j` 待决密码输入与 `400` 重试由 `tools_and_bitlocker` 无头集成测试实测，其余终态完成 `winapp` 截图核对 |
| `00F · 背景着色器`（`gcj97`） | `jzHSt` 天空与 `k6JMV` 夜空着色器绑定与静止渲染 | `[ ]` | 静态首帧已在 `WSbcS` 验证；切换停帧与低功耗回退待结合动效阶段实测 |

---

## 二、业务画板逐屏核查（`.pen` `01–07`）

### 01 · 工作台（`ityMT`）

- [x] `WSbcS` 主页（含 `p2HWHP` 展开态、`OeuCo` 收起态、`PuezS` 深色态、双圆弧标识、无阴影主标题与 `600×88` 输入框；已完成双主题 `winapp` 视觉核查、`h1BufF` 顶部通知定位实测及 `sol` 复核闭环）
- [ ] `WSbcS / xdA96` 主页 `X2gAh` 顶部报错条实机触发态截图验证（`h1BufF` 顶部通知条已在字体回退场景完成触发态验证）
- [x] `l84goO` 对话（含 `s0bIf`/`FY9tR` 对话排版、过程折叠头、Markdown 标题/表格/代码块/行内代码深浅色对比、`s4IJwS` 中断与报错详情、`ivhR0` 原比例灯箱、`qFaoI` 深色与本地 SSE 实时流式输出；已完成双主题 `winapp` 真窗核查，并通过 `sol` 工程复核及灯箱冒泡/尺寸/弹窗语义修复与复测）
- [ ] `l84goO / KLzVw & GM9WU` 长会话上滑出现 `40px` 黑底「回到底部」悬浮按钮与 `507` 本机失败用户气泡（`CircleAlert` 图标）真窗触发截图（静态结构已按 `.pen` 修正并通过编译，受限于 UIA 未暴露 `ScrollPattern` 尚待触发态截图）
- [x] `zdtJ6` 对话中的工具混排流与多工具折叠切换（已修正独立 `read` 图片脱离折叠段并保持折叠锚点 `expansion_anchor` 不串位，已完成双主题 `winapp` 与 `workbench_and_ime_ui` 单测验证）
- [ ] `Ed7xS` 终端（内置 TUI 视图切换、顶栏分段控件 `topbar-view-gui-btn` / `topbar-view-tui-btn`、窄幅与草稿衔接；阶段 10 待开发：`crates/cst-tui` 尚未创建，点击「终端」切换呈现占位说明「窗口内置终端视图将在阶段 10 开放。」）
- [x] `juqeU` 状态提示与软件更新（`h1BufF` 顶部通知、`WO6NQ` 更新/凭据缺省弹层、`pFooK` 退出确认「继续使用」/「停止任务并退出」与红标已完成双主题 `winapp` 截图核查；更新下载进度、取消与交接锁屏、焦点陷阱与降级草稿检查由 `settings_and_update` 与 `shell_and_mode_ui` 实测覆盖，无线上签名更新包实机截图）
- [x] `OmK5U` 输入与快捷面板（已对齐输入浮层 Z 轴叠放序、补全标题栏与 `/skill:` 分隔线及 `Ctrl+K` 底栏底色；`Ctrl+K` 面板、`/`/`@` 补全、上下文圆环额度弹层与双级模型/思考强度已完成双主题 `winapp` 截图核查；`queue`/`steer` 乐观重排回滚、IME/动作层键位、防重入与 `sol` 并发修补由 `workbench_and_ime_ui` 实测通过，队列卡未单采 `winapp` 实图）
- [ ] `KsR0v` 仪表盘（概览指标卡片、图表/配额布局与深浅色对比度；待开发占位：当前显示「本地用量仪表盘将在后续版本开放。」）

### 02 · 会话管理（`C49doB`）

- [x] `Fp7xb` 全部会话（自然日分组、长标题 `lzrMn` 列表行、`UWt8w`/`UyJBj` 右键菜单、重命名弹窗及空校验 `X2gAh`、`h5AY3` 侧栏多选与全部会话批量模式已完成双主题 `winapp` 截图核查及 `sol` 复核闭环；`409` 删除冲突回滚、失效多选清理与重命名竞态由 `settings_and_update` 与 `shell_and_mode_ui` 实测覆盖）
- [x] `bliwY` 分支树（五种过滤模式、节点间距与 `2px` 引导线、底部快捷键提示条、`c1wymw` 跳转总结弹窗已完成双主题 `winapp` 截图核查及 `sol` 复核闭环，并补正同会话分支跳转后激活刷新；`Ctrl+O`/方向键/IME 隔离与 `Esc` 逐层退回由 `settings_and_update` 实测覆盖）
- [x] `P7uGp` 会话导出（导出选项、格式/主题切换与 `CST Pilot` 预览区已完成双主题 `winapp` 截图核查；生成落盘通知、打开导出目录与离页取消轮询防误开目录由 `settings_and_update` 实测覆盖，真窗未拉起系统资源管理器）

### 03 · 设置（`Trond`）

- [x] `sxkCI` 常规（`VPWAF` 设置导航、外观浅/深/跟随系统切换、系统集成与更新状态行 `c4PWw` 已完成双主题 `winapp` 核查）
- [x] `t7zsNf` 模型服务（已核查列表与 `T5us8O` `已连接` 徽标、自定义网关 `q8NLwK` 增删改、`providers-apikey-input` 掩码及 1 级红框校验、凭据请求流内存清零及 `delete_custom_provider` 修复；真窗 `winapp` 与 `settings_and_update` 闭环）
- [x] `b9mFa` 模型偏好（已核查 `H2p2z` 默认模型/筛选毛玻璃浮层及 Z 轴叠放序、`search`/`refresh-cw`/`plus` 组头、`N 个已启用` 计数、乐观排序回滚及 `cst-server` 全量重排保留顺序；真窗 `winapp` 与 `settings_and_update` 闭环）
- [x] `i6TeGT` 快捷键（快捷键列表与 `ZCGS3` 键位胶囊已完成双主题 `winapp` 核查）

### 04 · 账号与登录（`ABJtA`）

- [x] `JN6Rt` 登录（已核查 `PHFud` 服务商毛玻璃下拉框、`login-apikey-input` 无回显填密、1 级 `$status-danger` 单框校验及输入清除、`Zeroizing` 序列化与 `blue-hour`/`night-sky` 渐变过渡；真窗 `winapp` 与 `settings_and_update` 闭环）
- [x] `T932X7` CSTOA 登录（已核查 `N1Rfg` 二维码状态条、`night-sky`/`blue-sky` 背景着色器双主题真窗表现；离线二维码光栅化、`deviceCode` 解析及轮询代际取消由 `settings_and_update` 实测，离线 `winapp` 夹具无在线扫码服务端）
- [x] `uV4C7` 账号（已通过模拟 `auth.json` 已登录夹具完成双主题 `winapp` 截图核查：`WzYef` 账户菜单、`c4PWw` 信息行、`GOWKl` 操作栏与 `sVucQ` `退出 CSTOA` 弹窗及 `DZ0Fp` 遮罩；实时额度接口由 `settings_and_update` 覆盖）

### 05 · 深色模式部分参考（`wZKdd`）与 06 · 边界情况（`S8xM0`）

- [ ] `x8G7D` 深色模式跨组件补检（联网检索 `JQMQZ`、命令清单 `s3WzY` 等深色卡片细节）
- [ ] `BqA38` 窗口尺寸与长文本极限边界（最小窗口、窄视口自动收起侧栏、超长会话名/工具路径截断、长代码块横向滚动与大段报错换行）

### 07 · 工具与特殊工具（`uF5B5`）

- [x] `kcHlA` 普通工具全量态（`VwO1J` 原文等宽块/对齐列、`read` 附图独立展开、`FMaqq`/`RvBpL` 降级与报错仅图标着色已完成双主题 `winapp` 核查；17 工具映射与 web 夹具对齐通过单测验证）
- [x] `k6vP8P` 整机概览细节与硬件保护态（已核查 `sys(overview)` 双主题 `详情` 展开列表、`counterErrors` 去重与降级提示；`sys(gpu)`/`sys(sensor)` 停帧由 `shader_frame_scheduler` 集成测试实测）
- [x] `i782jQ` BitLocker 特殊工具卡片（解密进度 `4px` 进度条、`60px` 盘符列、`unknown` 异常中断、已取消与解锁失败态已完成双主题 `winapp` 核查；待决 `ABn8j` 密码框 `bitlocker-pwd-input-*`、`400` 原位报错重试及 IME 隔离由 `tools_and_bitlocker` 无头集成测试实测，静态 `.jsonl` 夹具未覆盖活跃 `requests` 真窗截图）
- [x] `aT4ai` 命令清单卡片（`command_plan`/`runbook_deliver`/失败卡片、`打开` 蓝字外链与底部 `详情` 栏已完成双主题 `winapp` 核查；清单产物写入与打开回调由 `tools_and_bitlocker` 实测）
- [x] `Y5236` 联网检索卡片（三层中性投影 `craft_stack`、降级/失败图标分色与折叠查询列表已完成双主题 `winapp` 核查；悬浮提示条几何按源码 `16px/8px` 与 `4px` 圆角核对）

---

## 三、动效原语与交互行为核查（`design/storybook.html`）

### 21 项动效原语与曲线核查

- [ ] **基础按压与悬停组（`press` / `hover` / `focus`）**：`120ms ease-out` 按压缩放 `0.97`（复选框 `0.92`）、`150ms` 悬停底色与 `150ms` 焦点边阴影，全量支持可打断与减少动态 `0ms`
- [ ] **浮层与弹窗组（`popover` / `modal` / `toast`）**：快关慢开节奏（弹层 `220ms`/`150ms`、`modal` `240ms`/`150ms` 从 `scale(0.96)`、通知 `240ms`/`150ms` 及 `2600ms` 自动收起且悬停暂停）
- [ ] **展开与位移组（`collapse` / `chevron` / `flip` / `pageSlide` / `sidebarShift` / `drawer`）**：`240ms` 折叠与箭头旋转、列表重排 `flip`、`±8px` 切页、`24px` 侧栏与抽屉展开，**中途反向触发须从当前插值平滑折返，禁止重置回 `0`**
- [ ] **反馈与指示组（`checkStroke` / `iconSwap` / `segPill` / `tabIndicator`）**：勾选 `340ms` `--ease-spring`、复制图标缩放 `0.65 -> 1` 切换（禁止模糊）并于 `1600ms` 恢复、分段控件与选项卡滑块 `240ms ease-in-out`
- [ ] **零延迟与节律组（`tooltipGroup` / `welcomeStagger` / `streamAppend` / `runningLight` / `shaderFreeze`）**：百次级快捷键 `0ms` 瞬时响应；相邻悬停跳过 `400ms` 初始延迟；欢迎屏错位总时长 `<= 300ms` 且同会话站内返回不重播；流式追加不破坏文本选区且不重播容器动效；`RunningLight` `2000ms` 线性 5 字素扫光；隐藏/失焦/硬件采样期 `0 FPS` 停帧；全局 `减少动态` 切换验证

---

## 四、材质层级、系统规则与文案红线（`storybook.html` & `DESIGN.md`）

- [ ] **L0–L4 五层材质体系**：`L0` 主区与表单纯实底无毛玻璃；`L1` 仅模态弹窗启用 `DZ0Fp` 模糊遮罩；`L2` 三层中性投影无彩色溢光；`L3` `H2p2z`/`K3x72` 毛玻璃渲染参数与降透明实底回退；`L4` 顶光（`y=1px`）排在发丝内边之前防止压暗顶沿
- [ ] **`Esc` 焦点栈与光标规则**：连续按 `Esc` 每次仅关闭最顶层表面，并在 `1` 帧内将焦点归还原触发控件；内部交互统一系统箭头光标，仅外链与打开外部文件使用手型光标及下划线
- [ ] **输入法与双通道锁定**：输入框、搜索框与重命名框 IME 组合期内候选窗跟随且 `Enter`/`Esc`/`Tab`/方向键交还输入法；启动网页模式后全窗口覆盖 `WebActive` 锁定遮罩拦截写操作；存在运行中任务/队列/非空草稿关闭软件时触发退出确认并将默认焦点落在「继续使用」
- [ ] **三级异常分流与文案数据规范**：核查全界面 `1 / 2 / 3A / 3B` 四类异常无多处重复提示、标准三句式服务端报错、禁用感叹号与营销修辞、时间格式 `YYYY/MM/DD HH:mm`、耗时 `3.2s`、等宽数字 `tabular-nums` 及数字与单位间单半角空格
