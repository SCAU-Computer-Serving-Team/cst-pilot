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
    reason: 各画布只有局部取值，未定为令牌
  - section: rounded
    reason: 只有画布变量 radius-control / radius-row / radius-menu / radius-composer，未定义全站圆角语义
  - section: components
    reason: 组件 token 不在本文范围
---

# CST Pilot Web 设计系统

本文定义 Web 的颜色、排版、动效与跨页面视觉规则。页面布局和逐元素尺寸由画布承载；本文明确列出的视觉规则优先于画布示意。

产品行为、接口与实现安排见 [Web 文档](doc/web/README.md)，不由本文裁定。

| 参考 | 范围 |
|---|---|
| [cst-pilot-colors.pen](src/web/design/cst-pilot-colors.pen) | 色阶画布。取色的权威依据 |
| [cst-pilot-web.pen](src/web/design/cst-pilot-web.pen) | 界面画布。各页浅深两版的取值、尺寸与形态 |
| [cst-pilot-tools.pen](src/web/design/cst-pilot-tools.pen) | 工具调用画布。工具卡片与调用状态的取值、尺寸与形态 |
| [文件清单](#文件清单) | 视觉画布、资产与字体 |

格式依据 [DESIGN.md Format](https://github.com/google-labs-code/design.md)（alpha）。本文件放在仓库根目录，供 Pen 从工作目录读取。

## 文件清单

本节只索引视觉资产。产品与工程文档见 [Web 文档](doc/web/README.md)，工具设计见 [工具设计索引](doc/design/README.md)。

背景参数见 [Blue hour](src/web/design/asset/blue-hour.md) 与 [AIR](src/web/design/asset/blue-hour-air.md)，同类产品布局见[排版参考](src/web/design/asset/reference/README.md)。

### 画布

| 文件 | 内容 |
|---|---|
| [cst-pilot-colors.pen](src/web/design/cst-pilot-colors.pen) | 12 步用途、中性与四组彩色色阶 |
| [cst-pilot-web.pen](src/web/design/cst-pilot-web.pen) | 首页、聊天工作台、登录页与仪表盘的浅深两版用色、背景与排版；另含上下文面板、模型选择、思考强度、账号菜单、消息菜单的悬停态 |
| [cst-pilot-tools.pen](src/web/design/cst-pilot-tools.pen) | 工具调用画布。以「聊天工作台 · 浅色」一帧为参照，配工具卡片的三种通用渲染模式、「已发送（等待返回）」状态，以及 `sys overview`、`runbook` 两个例外；浅深两版齐备 |
| [heroui-colors.pen](src/web/design/heroui-colors.pen) | HeroUI v3 默认主题对照 |
| [asset/pencil-heroui.pen](src/web/design/asset/pencil-heroui.pen) | HeroUI 组件参考画布 |

### 资产

| 类别 | 文件 | 用途 |
|---|---|---|
| 令牌 | [cst-pilot-colors.tokens.json](doc/web/asset/cst-pilot-colors.tokens.json) | 五组 × 浅深共 120 个色值，DTCG 2025.10 |
| 生成器 | [make-color-scale.mjs](doc/web/asset/make-color-scale.mjs) | 由色相与明度曲线输出上表 |
| 对照 | [heroui.tokens.json](doc/web/asset/heroui.tokens.json)、[make-heroui-tokens.mjs](doc/web/asset/make-heroui-tokens.mjs) | HeroUI v3 默认主题原始变量与解析脚本，不是本项目的映射结果 |
| 导出图 | [cst-pilot-color-steps.png](doc/web/asset/cst-pilot-color-steps.png)、[cst-pilot-color-scale.png](doc/web/asset/cst-pilot-color-scale.png)、[cst-pilot-blue-hour.png](doc/web/asset/cst-pilot-blue-hour.png) | 三张色卡画布的导出，宽 2312 |
| 对照图 | [heroui-colors-core.png](doc/web/asset/heroui-colors-core.png)、[heroui-colors-light.png](doc/web/asset/heroui-colors-light.png)、[heroui-colors-dark.png](doc/web/asset/heroui-colors-dark.png) | `heroui-colors.pen` 三张画布的导出 |
| 色表 | [blue-hour-palette.png](src/web/design/asset/blue-hour-palette.png)、[blue-hour-dark-palette.png](src/web/design/asset/blue-hour-dark-palette.png)、[blue-hour-air-dark-palette.png](src/web/design/asset/blue-hour-air-dark-palette.png) | 256×1 色彩查找表 |
| Shader | [blue-hour.glsl](src/web/design/asset/blue-hour.glsl)、[blue-hour-air.glsl](src/web/design/asset/blue-hour-air.glsl) | 首页与登录页的动态背景 |
| 降级样式 | [blue-hour.css](src/web/design/asset/blue-hour.css) | 无 WebGL 时的 CSS 近似，不画板条 |
| 背景原图 | [Blue hour-3840x2160.svg](src/web/design/asset/Blue%20hour-3840x2160.svg)、[blue-hour-background.png](src/web/design/asset/blue-hour-background.png)、[blue-hour.png](src/web/design/asset/blue-hour.png) | 用户提供的原图与静态兼容图 |
| 主页快照 | [login-preview/](src/web/design/asset/login-preview/) | 登录页场景预览的模糊底层 |

### 字体

画布使用的字体族见 [字体分工](#字体分工)。字体文件不入开发仓库（三份 OTF 共 25 MB），发行版随 Web 通道提供，可由上游 OFL 发布物重建。`cst-pilot-web.pen` 按相对路径 `fonts/` 引用其中三份，本地缺失时画布回退到系统字体。

| 字体 | 文件 | 用途 |
|---|---|---|
| Source Han Sans CN（思源黑体） | `src/web/design/fonts/` 三档字重（Regular / Medium / Bold） | 欢迎语、中文导航、会话标题、分组文字、账户信息 |
| Inter | 无文件 | 产品名、模型名、页脚 |
| JetBrains Mono | 无文件 | 聊天页行内工具名 |
| Noto Sans SC | 无文件 | 仅 `cst-pilot-colors.pen` 的设计说明 |

## Overview

### 品牌标识

以下是现有品牌素材。Web 标识的用法与尺寸列入[资源待办](doc/issues.md#3-视觉与资源)。

| 文件 | 内容 |
|---|---|
| [assets/logo.png](assets/logo.png) | 主标识：`CST Pilot` 像素方块字形，2040×456，蓝白配色，用于仓库 README 插图 |
| [assets/make-logo.cjs](assets/make-logo.cjs) | 主标识生成脚本；只留在开发仓库，`pack` 明确排除，不随发行包分发 |
| [logo-original.png](src/web/design/asset/logo-original.png) | 主标识原图，与 `assets/logo.png` 同尺寸 |
| [logo-reversed.png](src/web/design/asset/logo-reversed.png) | 反白版，用于深色底 |
| [logo-reversed-accent.png](src/web/design/asset/logo-reversed-accent.png) | 反白版加强调色 |
| [logo-mark.png](src/web/design/asset/logo-mark.png) | 单独的 `C` 字形标记，209×293 |

四份 `logo-*.png` 是备用素材，未被画布引用。

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
| 文件格式 | DTCG 2025.10，后缀 `.tokens.json`；只能下层引用上层：`reference` 原始值 → `system` 语义角色 → `component` 组件值 |

正文与 front matter 的十六进制值对应画布使用的 sRGB。完整五组、浅深两套共 120 个色值及 OKLCH 数值见 [颜色令牌](doc/web/asset/cst-pilot-colors.tokens.json)，生成规则见 [生成器](doc/web/asset/make-color-scale.mjs)。front matter 的颜色键描述首页落点及彩色实心档，不代表已完成 HeroUI 变量映射。其中 `primary` 是色系 `accent` 的第 9 步实心色，`success-solid`、`warning-solid`、`danger-solid` 同理。

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

四组彩色沿用相同的 12 步用途。首页未展示成功、警告、危险状态，发送按钮也不使用通用主色第 9 步；仪表盘的状态徽标是唯一落到组件的彩色用法；色卡里的色块不是组件状态设计。

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
| 悬停底色 | `#EAEAEA` | 中性 4 | 浅色用无偏灰白，不用中性 4。侧栏选中态同值，浅色三页一致；深色选中态用中性 5 `#242C2F` |
| 消息输入框 | `#FFFFFF` | 中性 2 | 深色不用中性 3，避免与侧栏撞色 |
| 发送按钮 | `#465A9F` | `#7785DE` | Blue hour 家族，不用主色第 9 步 |
| 发送箭头 | `#FFFFFF` | `#001F42` | 深色取浅色主色阶第 12 步 |
| 页脚 `@cst-pilot-web` | 中性 11 | 中性 1 | 深色页脚压在底部亮色带上，用中性 1 才能读 |

品牌 C 的两条弧线使用白色透明度渐变描边：0% 处 `#FFFFFF00`、55% 处 `#FFFFFF1F`、100% 处 `#FFFFFFFF`，画布渐变旋转为 0°。它不是整条不透明白线。

#### 聊天页浅色：无色偏灰白

覆盖作用于「聊天工作台 · 浅色」与「登录页 · 浅色」的控件，不改主页与通用色阶。深色聊天页回通用色阶取色；深色标题栏取中性 2 `#0B1215` 的 95%（`#0B1215F2`），与主区同色，不用更黑的中性 1。

| 档 | 值 |
|---|---|
| 主区背景 | `#FFFFFF`，无背景图片、图案、渐变、纹理或 Shader |
| 标题栏 / 侧栏 | `#FFFFFFF2` / `#F6F6F6` |
| 输入框 / 表格正文底 | `#FFFFFF` |
| 用户气泡 / 表头 / 行内代码 | `#F0F0F0` |
| 选中会话 | `#EAEAEA` |
| 正文 / 次要文字与图标 | `#202020` / `#646464` |
| 分隔线 / 输入框边框 | `#E0E0E0` / `#D5D5D5` |
| 检索与工具图标 / 成功状态 | 主色 11 `#0665BE` / 成功 11 `#007B3A` |
| 停止按钮 / 图标 | `#465A9F` / `#FFFFFF`，保留品牌操作色 |
| 等待轨道 | `#E3ECF1`，旋转加载器的静态轨道，是本节唯一不用灰白的中性色 |

设备图标容器三个页面、两种主题都取中性 5，浅色 `#DBE4E9`、深色 `#242C2F`；图标在容器内水平与垂直居中。

「登录页 · 浅色」沿用同一族灰白：方式切换轨道 `#F0F0F0`、选中块 `#FFFFFF`、激活文字 `#202020`、未激活文字 `#646464`、二维码描边 `#E5E5E5`。深色登录页不沿用，改取通用色阶：轨道中性 4 `#1D2428`、选中块中性 5 `#242C2F`、未激活文字与扫码提示中性 11 `#AAB3B7`、正文 `#FFFFFF`。

#### 悬停面板

输入框操作栏弹出上下文面板、模型选择与思考强度三个；侧栏左下角弹出账号菜单；聊天标题栏右上角三个点弹出消息菜单。五个面板共用一套卡片式样式，除选中行底色与投影外全部取自通用色阶。

选中行浅色用 `#EAEAEA`，不用中性 5 `#DBE4E9`：后者带蓝调，在白面板上会读成淡蓝底。深色选中行用中性 5 `#242C2F`。

#### 投影

全站投影只用一个颜色：中性 12 `#1A2024`，靠透明度分档，不用纯黑，不带彩色。

| 落点 | 透明度 | 参数 |
|---|---|---|
| 悬停面板 | 8% | 偏移 0,8；blur 24 |
| 登录卡片 | 8% | 偏移 0,12；blur 40 |
| 登录页面板 · 近层 | 8% | 偏移 0,4；blur 12 |
| 登录页面板 · 远层 | 20% | 偏移 0,24；blur 64，spread -12 |
| 首页欢迎标语 | 25% | 只在 Pen 上示意，未定实现 |

#### 登录页面板

登录页在场景里是一个 1080×1080、圆角 32 的居中面板。面板底色不可见，实际由 AIR 背景铺满，可见的只有描边与投影。

| 部位 | 取值 |
|---|---|
| 面板描边 | 1.5px 三段线性渐变，自上而下 `#FFFFFFA6` → `#FFFFFF4D` → `#465A9F40` |
| 面板投影 | 见[投影](#投影) |
| 登录卡片 | 浅色白底 / 深色中性 3 `#131A1D`；圆角 24；描边中性 6（浅 `#D5DEE3` / 深 `#2D3438`） |
| 卡片内控件 | 方式切换与二维码取无色偏灰白，见[聊天页浅色](#聊天页浅色无色偏灰白) |
| 底部可读性渐变 | 主色浅色 4 `#DDECFF`；0%–70% 全透明，93% 起不透明 |
| 欢迎语 | 标题 56px/600 `#FFFFFF`；副题 16px `#FFFFFFCC` |
| 呈现效果帧的遮罩 | 中性 1 `#060B0E` 的 15%，即 `#060B0E26` |

#### 工具调用卡片

每个工具一次调用对应一张卡片，跟随聊天页取色，不另立色表。取值与状态见 [工具调用画布](src/web/design/cst-pilot-tools.pen)。

| 项 | 浅色 | 深色 |
|---|---|---|
| 卡片文字、标签与图标 | `#646464` | 中性 11 `#AAB3B7` |
| 原文块正文，唯一降一档的密集长文 | 中性 10 `#757D82` | 中性 9 `#7F888C` |
| 调用语句片与缩进竖线 | `#F0F0F0` | 中性 4 `#1D2428` |
| 原文块与命令灰块 | `#F6F6F6` | 中性 2 `#0B1215` |

语义色只给图标，不给边框和底色：成功用浅色 11 `#007B3A` / 深色 9 `#20A655`，警告用浅色 11 `#8C5A00` / 深色 9 `#ED9F26`，危险用浅色 11 `#BF091B` / 深色 9 `#F4232E`，主色用浅色 11 `#0665BE` / 深色 9 `#1B85F2`。

输出正文的字号按四层递减；四层与折叠结构的对应见[工具调用的折叠](doc/web/SPEC/chat-workspace.md#工具调用的折叠)。正文内其他内容的字号一律引用这四层，不另立数值。

| 层 | 字号 | 适用范围 |
|---|---|---|
| 第 1 层 · 正文 | 16px | LLM 回复正文、技术行（折叠计数行「正在调用多个工具…」「已完成检查 · N 次工具调用」）、联网检索行 |
| 第 2 层 · 状态行 | 13px | 折叠工具行「正在调用 `disk` 工具…」、卡片状态行（状态图标 + 调用文本 + 耗时） |
| 第 3 层 · 卡片内容 | 12px | 概览组、排行组、提示条、调用语句片 |
| 第 4 层 · 密集长文 | 11px | 原文块正文（长文本、代码、日志） |

加载指示圆弧直径等于同行字号，颜色与同行文字一致。例外工具（`sys overview`、`runbook`、`web_search`、`read` 图片形态）不进入折叠：状态行按第 1 层 16px 呈现，展开内容仍为第 3 层 12px、第 4 层 11px。`sys overview` 与 `runbook` 骨架同普通卡片，差别见[诊断卡片](doc/web/SPEC/diagnostic-cards.md#渲染规则)；`web_search` 单独占一行，展开仅显示传入关键词（12px），不弹出返回内容，完成以图标标识，见[web_search 卡片](doc/design/web/tool/web_search.md)；`read` 读取图片时先展示图片，再展示绝对路径，见[read 卡片](doc/design/web/tool/read.md)。

字段行数值列按整张卡片对齐：取卡内最长的字段名宽度，加 10px 间距作为数值列起点，所有分组的字段行共用同一列。

卡片正文、字段名与字段值、分组标题用思源黑体（分组标题 500，其余 400）；调用语句片与耗时用 JetBrains Mono；卡片内不用 Inter。

#### 动态背景

Blue hour（首页）与 AIR（登录页）独立于通用色阶，各自使用独立色表与参数；浅色背景不能原样复用于深色主题。参数、来源与算法差异见 [Blue hour 背景](src/web/design/asset/blue-hour.md) 与 [登录页 AIR 背景](src/web/design/asset/blue-hour-air.md)。两处背景只在画布上，网页动画与兼容版均未实现。

### 对比度边界

正文和控件文字以 WCAG AA 4.5:1 为目标。第 9 步不是「可以直接配白字」的保证；当前主色、成功、警告、危险第 9 步与白色的对比度分别约为 3.69、3.16、2.19、4.10，均未达到普通文字要求。

后续制作语义按钮时需单独确定前景色或调整背景。动态背景上的文字需检查多个动画时刻；全量对比度验收未完成。

## Typography

### 字体分工

网页正文与控件使用思源黑体和 Inter；聊天页行内工具名使用 JetBrains Mono。色卡里的 Noto Sans SC 仅用于设计说明。字体文件清单见 [字体](#字体)。

| 字体 | 画布中的用途 |
|---|---|
| `Source Han Sans CN`（思源黑体） | 欢迎语、中文导航、会话标题、分组文字、输入提示，以及当前账户信息 |
| `Inter` | 产品名 `CST Pilot`、模型名称、页脚 `@cst-pilot-web` |
| `JetBrains Mono` | 聊天页行内的工具名 |
| `Noto Sans SC` | 仅用于 `cst-pilot-colors.pen` 的标题、注释和色值说明 |

字体按界面角色指定，不按每个字符自动切换。例如账户名 `Tim2354` 在当前画布中仍使用思源黑体。网页字体分发、加载与缺字回退方案见[资源待办](doc/issues.md#3-视觉与资源)。

### 行高与字距

首页不设行高与字距，用字体默认值。其余取值只在下列范围成立，不跨场景套用。

| 范围 | 值 |
|---|---|
| 聊天页正文与控件 | `lineHeight: 1.5` |
| 聊天页表格单元格 | `lineHeight: 1.45` |
| 工具卡片原文块 | `lineHeight: 1.6` |
| 仪表盘 36px 指标数值 | `letterSpacing: -0.6` |

### 使用边界

字号与字重以画布为准，front matter 的 `typography` 键只收录已出现的元素，不代表全站阶梯。

- 行高与字距只按[行高与字距](#行高与字距)取值，不从截图估算为固定令牌。
- 56px/600 只用于首页欢迎语，不直接推广为对话正文或普通页面标题。
- 15px 与 16px 的分组字号差异按画布执行；10px 只用于账户说明，不作全站最小字号。

## 动效

全站动效采用 [transitions.dev](https://transitions.dev/skill) 的令牌刻度与规则，不自行定数值。该站提供两个 skill，已全局安装：`transitions-dev` 从零做动效，`transitions-polish` 把已有动效对齐到刻度。

**授权待确认。** transitions.dev 的 GitHub 仓库未声明许可证，站点区分免费与 Pro。skill 可安装使用；把 `_root.css` 或过渡片段复制进发行包前，需先与站点条款核对。

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

**悬停进快、出软。** 进入 250ms 以内、用 `--ease-smooth-out`；退出可以更长、可以带弹性，使退出过程平缓。

**错位与延迟。** 错位间隔 40ms，少量大块元素用 80ms；总时长（间隔 × 项数）控制在 300ms 以内，列表长就减小间隔或限制错位项数。提示出现与成功勾选的路径有一段 80ms 的意图延迟，用来过滤误触。动效显得迟就减时长，不加延迟；关闭与悬停退出永不加延迟。

**位移过 40px 会显迟钝。** 除整块面板与抽屉外，都向 `--distance-base`（8px）收。

**无障碍。** 每条过渡都要带 `prefers-reduced-motion` 降级。Pen 画布不读取系统偏好，背景动画需手动把速度设为 0，见 [Blue hour 背景](src/web/design/asset/blue-hour.md)。

## 页面

各页取值、尺寸与形态直接看 [界面画布](src/web/design/cst-pilot-web.pen)，本文只记画布上看不出来的规则。登录页现有扫码态画布；API KEY 态待补，沿用登录页的背景与面板视觉规则。

| 页面 | 画布帧 | 本文只记录的规则 |
|---|---|---|
| 主页 | 「主页 · 浅色」「主页 · 深色」 | 例外规则见 [例外规则](#例外规则) |
| 聊天工作台 | 「聊天工作台 · 浅色」「聊天工作台 · 深色」 | — |
| 登录页 | 「登录页 · 浅色」「登录页 · 深色」，以及两个「呈现效果」帧 | 以 1080×1080 居中面板使用，配色、描边与投影见[登录页面板](#登录页面板)；背后主页模糊，面板保持清晰。两个「呈现效果」帧是面板 80% 等比副本，只作设计预览；正式实现模糊真实主页 |
| 仪表盘 | 「仪表盘 · 浅色」「仪表盘 · 深色」 | 表头与徽标等比所在表面深一档；分页当前页以中性 12 作底、字取所在表面色；图表映射为输入主色 9、缓存读取主色 8、输出成功 9、缓存写入警告 9；状态徽标用彩色浅底配第 11 步文字，取消状态用中性色；圆角沿用 `radius-control=6` / `radius-row=8` / `radius-menu=12`，不使用胶囊式控件；图表色标 3px 与堆叠柱顶角 5px / 4px 是字面值，不进变量 |

画布尺寸只描述该画布，不代表全站间距令牌或响应式断点。仪表盘的字段来源、费用与计时口径见 [仪表盘规格](doc/web/SPEC/dashboard.md)。
