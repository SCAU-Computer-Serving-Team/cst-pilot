---
version: alpha
name: CST Pilot Web
description: 计维队便携诊断工具 cst-pilot 的 Web 操作界面设计系统
colors:
  light-text: "#1A2024"
  dark-text: "#E0EAEE"
  light-muted: "#5F676C"
  dark-muted: "#AAB3B7"
  light-border: "#D5DEE3"
  dark-border: "#2D3438"
  light-hover: "#EAEAEA"
  dark-hover: "#1D2428"
  light-selected: "#DBE4E9"
  dark-selected: "#242C2F"
  light-sidebar: "#F5F6F8"
  dark-sidebar: "#131A1D"
  light-composer: "#FFFFFF"
  dark-composer: "#0B1215"
  light-send: "#465A9F"
  dark-send: "#7785DE"
  light-send-icon: "#FFFFFF"
  dark-send-icon: "#001F42"
  light-footer: "#5F676C"
  dark-footer: "#060B0E"
  welcome: "#FFFFFF"
  primary: "#1B85F2"
  success-solid: "#20A655"
  warning-solid: "#ED9F26"
  danger-solid: "#F4232E"
typography:
  welcome:
    fontFamily: "Source Han Sans CN"
    fontSize: "56px"
    fontWeight: 600
  product-name:
    fontFamily: "Inter"
    fontSize: "20px"
    fontWeight: 600
  navigation:
    fontFamily: "Source Han Sans CN"
    fontSize: "19px"
    fontWeight: 400
  conversation:
    fontFamily: "Source Han Sans CN"
    fontSize: "15px"
    fontWeight: 400
  input-placeholder:
    fontFamily: "Source Han Sans CN"
    fontSize: "20px"
    fontWeight: 400
  model-selector:
    fontFamily: "Inter"
    fontSize: "18px"
    fontWeight: 400
  footer:
    fontFamily: "Inter"
    fontSize: "15px"
    fontWeight: 400
omitted:
  - section: spacing
    reason: 间距比例未讨论；各画布只有局部取值
  - section: rounded
    reason: 只有画布变量 radius-control / radius-row / radius-menu / radius-composer，未定义全站圆角语义
  - section: components
    reason: 组件 token 未讨论
---

# CST Pilot Web 设计系统

本文是设计规范的唯一真源，只定「怎么取色、怎么排版」这一层。逐元素的取值、尺寸与形态由画布承载，本文不复述。冲突时以本文为准。

本文不写 Layout、Elevation & Depth、Shapes、Components 四个章节：它们的内容只在画布上，尚未归纳成全站规则，取值直接看 [界面画布](web/design/cst-pilot-web.pen)。

| 参考 | 范围 |
|---|---|
| [cst-pilot-colors.pen](web/design/cst-pilot-colors.pen) | 色阶画布。取色的权威依据 |
| [cst-pilot-web.pen](web/design/cst-pilot-web.pen) | 界面画布。各页浅深两版的取值、尺寸与形态 |
| [文件清单](#文件清单) | 设计相关的全部文档、画布、资产与字体 |

格式依据 [DESIGN.md Format](https://github.com/google-labs-code/design.md)（alpha）。本文件放在仓库根目录，供 Pen 从工作目录读取。

## 文件清单

设计相关的文档、画布与资产全部列在本节，其他文档引用本节，不各自再列一份。

### 文档

| 文件 | 内容 |
|---|---|
| [Web 技术决策](doc/design/web/tech-decisions.md) | 产品方向、承载与启动、技术栈、会话交互、只读边界、开发分发、架构阶段待办 |
| [设计标准](doc/design/web/design-standards.md) | 组件库、令牌格式与分层、字号与动效标准、兼容版降级 |
| [浏览器支持](doc/design/web/browser-support.md) | 两档内核基线、选档命令、支持清单与现场策略 |
| [仪表盘](doc/design/web/dashboard.md) | 字段来源、费用与计时口径、Pi 接口核对 |
| [Pen 操作建议](doc/design/web/pen.md) | Pen CLI 与 MCP 的使用注意事项 |
| [技术调研](doc/design/web/tech-research.md) | 降级能力分层、pi 多消费者、兼容版实测 |
| [工具设计](doc/design/tool/) | `sys`、`driver`、`eventlog` 三个工具的设计理由 |
| [Blue hour 背景](web/design/asset/blue-hour.md) | 首页 PRISM2 板条的色表、参数与来源 |
| [登录页 AIR 背景](web/design/asset/blue-hour-air.md) | AIR 椭圆布局、控制参数与居中面板 |
| [排版参考](web/design/asset/reference/README.md) | 同类 AI 对话产品的界面截图与共性结论 |

### 画布

| 文件 | 内容 |
|---|---|
| [cst-pilot-colors.pen](web/design/cst-pilot-colors.pen) | 12 步用途、中性与四组彩色色阶 |
| [cst-pilot-web.pen](web/design/cst-pilot-web.pen) | 首页、聊天工作台、登录页与仪表盘的浅深两版用色、背景与排版；另含上下文面板、模型选择、思考强度、账号菜单、消息菜单的悬停态 |
| [heroui-colors.pen](web/design/heroui-colors.pen) | HeroUI v3 默认主题对照 |
| [asset/pencil-heroui.pen](web/design/asset/pencil-heroui.pen) | HeroUI 组件参考画布 |
| [achieved/](web/design/achieved/) | 归档：配色方案 B、C 的画布与导出图，已被 `cst-pilot-colors.pen` 取代 |

### 资产

| 类别 | 文件 | 用途 |
|---|---|---|
| 令牌 | [cst-pilot-colors.tokens.json](doc/design/web/asset/cst-pilot-colors.tokens.json) | 五组 × 浅深共 120 个色值，DTCG 2025.10 |
| 生成器 | [make-color-scale.mjs](doc/design/web/asset/make-color-scale.mjs) | 由色相与明度曲线输出上表 |
| 对照 | [heroui.tokens.json](doc/design/web/asset/heroui.tokens.json)、[make-heroui-tokens.mjs](doc/design/web/asset/make-heroui-tokens.mjs) | HeroUI v3 默认主题原始变量与解析脚本，不是本项目的映射结果 |
| 导出图 | [cst-pilot-color-steps.png](doc/design/web/asset/cst-pilot-color-steps.png)、[cst-pilot-color-scale.png](doc/design/web/asset/cst-pilot-color-scale.png)、[cst-pilot-blue-hour.png](doc/design/web/asset/cst-pilot-blue-hour.png) | 三张色卡画布的导出，宽 2312 |
| 对照图 | [heroui-colors-core.png](doc/design/web/asset/heroui-colors-core.png)、[heroui-colors-light.png](doc/design/web/asset/heroui-colors-light.png)、[heroui-colors-dark.png](doc/design/web/asset/heroui-colors-dark.png) | `heroui-colors.pen` 三张画布的导出 |
| 色表 | [blue-hour-palette.png](web/design/asset/blue-hour-palette.png)、[blue-hour-dark-palette.png](web/design/asset/blue-hour-dark-palette.png)、[blue-hour-air-dark-palette.png](web/design/asset/blue-hour-air-dark-palette.png) | 256×1 色彩查找表 |
| Shader | [blue-hour.glsl](web/design/asset/blue-hour.glsl)、[blue-hour-air.glsl](web/design/asset/blue-hour-air.glsl) | 首页与登录页的动态背景 |
| 降级样式 | [blue-hour.css](web/design/asset/blue-hour.css) | 无 WebGL 时的 CSS 近似，不画板条 |
| 背景原图 | [Blue hour-3840x2160.svg](web/design/asset/Blue%20hour-3840x2160.svg)、[blue-hour-background.png](web/design/asset/blue-hour-background.png)、[blue-hour.png](web/design/asset/blue-hour.png) | 用户提供的原图与静态兼容图 |
| 主页快照 | [login-preview/](web/design/asset/login-preview/) | 登录页场景预览的模糊底层 |

### 字体

画布使用的字体族见 [字体分工](#字体分工)。字体文件不入开发仓库（三份 OTF 共 25 MB），发行版随 Web 通道提供，可由上游 OFL 发布物重建。`cst-pilot-web.pen` 按相对路径 `fonts/` 引用其中三份，本地缺失时画布回退到系统字体。

| 字体 | 文件 | 用途 |
|---|---|---|
| Source Han Sans CN（思源黑体） | `web/design/fonts/` 三档字重（Regular / Medium / Bold） | 欢迎语、中文导航、会话标题、分组文字、账户信息 |
| Inter | 无文件 | 产品名、模型名、页脚 |
| JetBrains Mono | 无文件 | 聊天页行内工具名 |
| Noto Sans SC | 无文件 | 仅 `cst-pilot-colors.pen` 的设计说明 |

## Overview

### 品牌标识

当前只有 TUI 的品牌标识，Web 端标识未定。

| 文件 | 内容 |
|---|---|
| [assets/logo.png](assets/logo.png) | 主标识：`CST Pilot` 像素方块字形，2040×456，蓝白配色，用于仓库 README 插图 |
| [assets/make-logo.cjs](assets/make-logo.cjs) | 主标识生成脚本；只留在开发仓库，`pack` 明确排除，不随发行包分发 |
| [logo-original.png](web/design/asset/logo-original.png) | 主标识原图，与 `assets/logo.png` 同尺寸 |
| [logo-reversed.png](web/design/asset/logo-reversed.png) | 反白版，用于深色底 |
| [logo-reversed-accent.png](web/design/asset/logo-reversed-accent.png) | 反白版加强调色 |
| [logo-mark.png](web/design/asset/logo-mark.png) | 单独的 `C` 字形标记，209×293 |

四份 `logo-*.png` 尚未被任何画布或文档引用，只作为素材留存。Web 端的标识形态、用法和尺寸随界面设计确定。

## Colors

### 组织方式

颜色分为通用 12 步色阶和首页专用配色。前者用于界面状态，后者用于背景、品牌与首页控件；两者不互相替代。

| 项 | 约定 |
|---|---|
| 语义角色 | 沿用 HeroUI v3；无后缀为背景，`-foreground` 为其上的文字或图标 |
| 色阶用途 | 参考 Radix Colors 的 12 步组织方式，不直接使用其色值 |
| 中性色 | 偏蓝；OKLCH 色相 230°，目标色度 0.012 |
| 彩色色相 | 主色蓝 254°、成功绿 151°、警告琥珀 72°、危险红 26° |
| 生成 | 浅深两套独立明度曲线；彩色峰值色度系数 0.94；超出 sRGB 色域时保留明度与色相、压低色度 |
| 文件格式 | DTCG 2025.10；`reference` 原始值 → `system` 语义角色 → `component` 组件值 |

正文与 front matter 的十六进制值对应画布使用的 sRGB。完整五组、浅深两套共 120 个色值及 OKLCH 数值见 [颜色令牌](doc/design/web/asset/cst-pilot-colors.tokens.json)，生成规则见 [生成器](doc/design/web/asset/make-color-scale.mjs)。front matter 的颜色键描述首页落点及彩色实心档，不代表已完成 HeroUI 变量映射。其中 `primary` 是色系 `accent` 的第 9 步实心色，`success-solid`、`warning-solid`、`danger-solid` 同理。

### 12 步用途与中性色

先按用途选步数，再按主题取色。深色主题不通过反转浅色颜色生成。

| 步 | 用途 | 浅色中性 | 深色中性 |
|---|---|---|---|
| 1 | 页面背景、画布底色 | `#FAFDFF` | `#060B0E` |
| 2 | 次级背景、卡片、隔行 | `#F2FAFF` | `#0B1215` |
| 3 | 按钮与输入框默认底色 | `#E8F1F6` | `#131A1D` |
| 4 | 悬停底色 | `#E3ECF1` | `#1D2428` |
| 5 | 按下与选中底色 | `#DBE4E9` | `#242C2F` |
| 6 | 弱边框、静态分隔线 | `#D5DEE3` | `#2D3438` |
| 7 | 输入框与卡片边框 | `#CDD6DB` | `#3C4448` |
| 8 | 强边框、键盘焦点环 | `#C5CED2` | `#4F575B` |
| 9 | 实心色块、主按钮、徽标 | `#7F888C` | `#7F888C` |
| 10 | 实心色块悬停 | `#757D82` | `#8B9498` |
| 11 | 次要说明、占位符 | `#5F676C` | `#AAB3B7` |
| 12 | 正文、标题、主要图标 | `#1A2024` | `#E0EAEE` |

### 彩色语义

四组彩色沿用相同的 12 步用途。首页未展示成功、警告、危险状态，发送按钮也不使用通用主色第 9 步；仪表盘的状态徽标是目前唯一落到组件的彩色用法，不能把色卡中的色块当成已完成的组件状态设计。

| 色系 | 第 9 步实心色（浅深相同） | 第 11 步文字：浅色 / 深色 | 第 12 步文字：浅色 / 深色 |
|---|---|---|---|
| 主色 `accent` | `#1B85F2` | `#0665BE` / `#77B4FF` | `#001F42` / `#D9EAFF` |
| 成功 `success` | `#20A655` | `#007B3A` / `#67CA83` | `#00270E` / `#BAFAC8` |
| 警告 `warning` | `#ED9F26` | `#8C5A00` / `#E4A247` | `#2E1A00` / `#FFE3C1` |
| 危险 `danger` | `#F4232E` | `#BF091B` / `#FF8A80` | `#430003` / `#FFDFDC` |

### 例外规则

通用色阶之外有三处约定。它们只在指定范围内生效，不回流到色阶。

#### 首页专用配色

| 项 | 浅色 | 深色 | 规则 |
|---|---|---|---|
| 侧栏底色 | `#F5F6F8` | 中性 3 | 浅色用独立灰白，不用中性 1 |
| 悬停底色 | `#EAEAEA` | 中性 4 | 浅色用无偏灰白，不用中性 4 |
| 消息输入框 | `#FFFFFF` | 中性 2 | 深色不用中性 3，避免与侧栏撞色 |
| 发送按钮 | `#465A9F` | `#7785DE` | Blue hour 家族，不用主色第 9 步 |
| 发送箭头 | `#FFFFFF` | `#001F42` | 深色取浅色主色阶第 12 步 |
| 页脚 `@cst-pilot-web` | 中性 11 | 中性 1 | 深色页脚压在底部亮色带上，用中性 1 才能读 |

品牌 C 的两条弧线使用白色透明度渐变描边：0% 处 `#FFFFFF00`、55% 处 `#FFFFFF1F`、100% 处 `#FFFFFFFF`，画布渐变旋转为 0°。它不是整条不透明白线。

#### 聊天页浅色：无色偏灰白

覆盖仅作用于「聊天工作台 · 浅色」，不改主页、登录页与通用色阶。深色聊天页回通用色阶取色。

| 档 | 值 |
|---|---|
| 主区背景 | `#FFFFFF`，无背景图片、图案、渐变、纹理或 Shader |
| 标题栏 / 侧栏 | `#FFFFFFF2` / `#F6F6F6` |
| 输入框 / 表格正文底 | `#FFFFFF` |
| 用户气泡 / 表头 / 行内代码 | `#F0F0F0` |
| 选中会话 / 设备图标容器 | `#EAEAEA` / `#E5E5E5` |
| 正文 / 次要文字与图标 | `#202020` / `#646464` |
| 分隔线 / 输入框边框 | `#E0E0E0` / `#D5D5D5` |
| 检索与工具图标 / 成功状态 | 主色 11 `#0665BE` / 成功 11 `#007B3A` |
| 停止按钮 / 图标 | `#465A9F` / `#FFFFFF`，保留品牌操作色 |

#### 悬停面板

输入框操作栏弹出上下文面板、模型选择与思考强度三个；侧栏左下角弹出账号菜单；聊天标题栏右上角三个点弹出消息菜单。五个面板共用一套卡片式样式，除选中行底色外全部取自通用色阶。

选中行浅色用 `#EAEAEA`，不用中性 5 `#DBE4E9`：后者带蓝调，在白面板上会读成淡蓝底。

#### 动态背景

Blue hour（首页）与 AIR（登录页）独立于通用色阶，各自使用独立色表与参数；浅色背景不能原样复用于深色主题。参数、来源与算法差异见 [Blue hour 背景](web/design/asset/blue-hour.md) 与 [登录页 AIR 背景](web/design/asset/blue-hour-air.md)。两处背景目前只用于 Pen，不能据此认定网页动画或兼容版已经实现。

### 对比度边界

正文和控件文字以 WCAG AA 4.5:1 为目标。第 9 步不是「可以直接配白字」的保证；当前主色、成功、警告、危险第 9 步与白色的对比度分别约为 3.69、3.16、2.19、4.10，均未达到普通文字要求。

后续制作语义按钮时需单独确定前景色或调整背景。动态背景上的文字需检查多个动画时刻；当前画布未完成全量对比度验收，本次记录不改变已有配色。

## Typography

### 字体分工

网页正文与控件使用思源黑体和 Inter；聊天页行内工具名使用 JetBrains Mono。色卡里的 Noto Sans SC 仅用于设计说明。字体文件清单见 [字体](#字体)。

| 字体 | 画布中的用途 |
|---|---|
| `Source Han Sans CN`（思源黑体） | 欢迎语、中文导航、会话标题、分组文字、输入提示，以及当前账户信息 |
| `Inter` | 产品名 `CST Pilot`、模型名称、页脚 `@cst-pilot-web` |
| `JetBrains Mono` | 聊天页行内的工具名 |
| `Noto Sans SC` | 仅用于 `cst-pilot-colors.pen` 的标题、注释和色值说明 |

字体按界面角色指定，不按每个字符自动切换。例如账户名 `Tim2354` 在当前画布中仍使用思源黑体。网页字体文件、加载策略和缺字回退字体尚未确定。

### 使用边界

字号与字重以画布为准，front matter 的 `typography` 键只收录已出现的元素，不代表全站阶梯。

- 首页文本未显式设置行高和字距，使用字体默认值；不从截图估算为固定令牌。
- 56px/600 只用于首页欢迎语，不直接推广为对话正文或普通页面标题。
- 15px/16px 的分组字号差异按画布保留，尚未统一；10px 账户说明是当前稿件值，不作为全站最小字号规范。
- 全站角色名称采用 `display / headline / title / body / label`，各分 `large / medium / small`；完整数值和首页元素的角色映射尚未确定。

## 动效

全站动效采用 [transitions.dev](https://transitions.dev/skill) 的令牌刻度与规则，不自行定数值。该站提供两个 skill，已全局安装：`transitions-dev` 从零做动效，`transitions-polish` 把已有动效对齐到刻度。实现时把 skill 里的 `_root.css` 导入一次，全局引用其中的 CSS 变量。

本节只定令牌与规则。具体用哪条过渡（下拉、模态、骨架屏等）由 skill 按界面元素决定，不在此列举。

### 令牌刻度

**时长**

| 令牌 | 值 | 用途 |
|---|---|---|
| `--duration-stagger` | 40ms | 逐项错位的间隔 |
| `--duration-micro` | 80ms | 提示延迟、抖动分段、大间隔错位 |
| `--duration-quick` | 150ms | 模态与下拉关闭、文字切换、提示出现 |
| `--duration-fast` | 250ms | 图标切换、下拉与模态打开、标签滑动、页面横向切换 |
| `--duration-medium` | 350ms | 面板关闭、toast 关闭 |
| `--duration-slow` | 400ms | 面板打开、骨架屏内容显现、输入框清空 |
| `--duration-very-slow` | 500ms | 强调时刻、徽标出现、文字渐显、成功勾选 |

**缓动**

| 令牌 | 值 | 用途 |
|---|---|---|
| `--ease-smooth-out` | `cubic-bezier(0.22, 1, 0.36, 1)` | 模态、下拉、面板的开与关，页面滑动，尺寸与位置变化 |
| `--ease-in-out` | `ease-in-out` | 图标切换、文字切换、文字渐显、骨架屏显现 |
| `--ease-out` | `ease-out` | 提示的开与关 |
| `--ease-linear` | `linear` | 流光、骨架屏脉冲、转圈 |
| `--ease-bounce` | `cubic-bezier(0.34, 1.36, 0.64, 1)` | 徽标弹出 |
| `--ease-bounce-strong` | `cubic-bezier(0.34, 3.85, 0.64, 1)` | 带弹性的悬停退出 |

**位移**

| 令牌 | 值 | 用途 |
|---|---|---|
| `--distance-micro` | 4px | 文字切换 |
| `--distance-small` | 6px | 抖动小段 |
| `--distance-base` | 8px | 徽标斜向出现、页面滑动、抖动大段 |
| `--distance-medium` | 12px | 文字渐显 |
| `--distance-large` | 30px | 勾选徽标出现 |

**缩放**

| 令牌 | 值 | 用途 |
|---|---|---|
| `--scale-large` | 0.96 | 模态开合 |
| `--scale-medium` | 0.97 | 下拉打开 |
| `--scale-small` | 0.98 | 提示打开 |
| `--scale-tiny` | 0.99 | 下拉关闭 |

**模糊**

| 令牌 | 值 | 用途 |
|---|---|---|
| `--blur-small` | 2px | 面板显现、图标切换、文字切换、骨架屏显现、数字跳动 |
| `--blur-medium` | 3px | 页面滑动、文字渐显 |
| `--blur-large` | 8px | 成功勾选打开 |

### 取值规则

**按用途选令牌，不按最接近的数值。** 300ms 的模态关闭照取 `--duration-quick`（150ms），因为两者的用途都是「模态关闭」。找不到用途对得上的令牌就不改，不硬套最近的数值。

**开关不对称，关闭比打开更快、更安静。**

| 对象 | 打开 | 关闭 |
|---|---|---|
| 下拉、模态 | 250ms | 150ms |
| 面板 | 400ms | 350ms |
| toast | — | 350ms |

两者对称、不拆开的：页面横向切换、标签滑动、手风琴、图标切换、文字切换。回弹缓动只用于入场（徽标弹出、数字跳动），关闭不用回弹；多数开关共用 `--ease-smooth-out`。

**悬停进快、出软。** 进入 250ms 以内、用 `--ease-smooth-out`；退出可以更长、可以带弹性，让它落定而不是弹断。

**错位与延迟。** 错位间隔 40ms，少量大块元素用 80ms；总时长（间隔 × 项数）控制在 300ms 以内，列表长就减小间隔或限制错位项数。提示出现与成功勾选的路径有一段 80ms 的意图延迟，用来过滤误触，不是给慢动效留余地。动效显得迟就减时长，不加延迟；关闭与悬停退出永不加延迟。

**位移过 40px 会显迟钝。** 除整块面板与抽屉外，都向 `--distance-base`（8px）收。

**无障碍。** 每条过渡都要带 `prefers-reduced-motion` 降级。Pen 画布不读取系统偏好，背景动画需手动把速度设为 0，见 [Blue hour 背景](web/design/asset/blue-hour.md)。

## 页面

各页取值、尺寸与形态直接看 [界面画布](web/design/cst-pilot-web.pen)，本文只记画布上看不出来的规则。

| 页面 | 画布帧 | 本文只记录的规则 |
|---|---|---|
| 主页 | 「主页 · 浅色」「主页 · 深色」 | 例外规则见 [例外规则](#例外规则) |
| 聊天工作台 | 「聊天工作台 · 浅色」「聊天工作台 · 深色」 | 不显示「本机 · 只读诊断」标签、独立 SMART 权限提示与回答操作栏的「仅检查」说明；真实诊断结果保留在正文表格 |
| 登录页 | 「登录页 · 浅色」「登录页 · 深色」，以及两个「呈现效果」帧 | 以 1080×1080 居中面板使用，32px 圆角、1.5px 内描边与柔和投影；背后主页模糊，面板保持清晰。「呈现效果」帧是面板 80% 等比副本，仅用于设计预览，正式实现模糊真实主页而非加载快照 |
| 仪表盘 | 「仪表盘 · 浅色」「仪表盘 · 深色」 | 表头与徽标等比所在表面深一档；分页当前页以中性 12 作底、字取所在表面色；图表映射为输入主色 9、缓存读取主色 8、输出成功 9、缓存写入警告 9；状态徽标用彩色浅底配第 11 步文字，取消状态用中性色；圆角沿用 `radius-control=6` / `radius-row=8` / `radius-menu=12`，不使用胶囊式控件 |

画布尺寸只描述该画布，不代表全站间距令牌或响应式断点。仪表盘的字段来源、费用与计时口径见 [仪表盘说明](doc/design/web/dashboard.md)。
