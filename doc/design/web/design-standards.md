# 设计标准

## 真源

仓库根的 `DESIGN.md` 是唯一真源。令牌文件与色卡图与它对齐，冲突时以它为准；HeroUI 变量的映射尚未完成。本文件只记大方向，同步方式见[设计文档索引](../README.md)。

## 组件系统

双版本技术栈与构建配对见 [tech-decisions.md](tech-decisions.md) 前端技术栈。HeroUI v3 要求 React 19 与 Tailwind v4，与现代版一致。

| 项 | 结论 |
|---|---|
| 组件库 | HeroUI v3（`@heroui/react` + `@heroui/styles` 3.2.5） |
| 现代版 | 直接用 HeroUI v3 |
| 兼容版 | 复用 v3 的 `heroui.min.css` 做 CSS 降级；组件用 React 18 自己实现，DOM 结构与类名跟 v3 一致 |

## 设计标准

规定「怎么写」，不含具体值。已确定的取值集中在根目录 `DESIGN.md`。

### 令牌格式

规定令牌怎么存进文件。只管文件长什么样，不管取什么颜色。

用 DTCG 2025.10，后缀 `.tokens.json`。

| 项 | 结论 |
|---|---|
| 校验 | 官方 JSON Schema `https://www.designtokens.org/schemas/2025.10/format.json` |
| 位置 | `doc/design/web/asset/` |

### 令牌分层

规定变量的层次与引用方向，让换主题只改一层的值，不动组件代码。

只能下层引用上层：

| 层 | 内容 | 例子 |
|---|---|---|
| reference | 原始值 | `--neutral-9` |
| system | 语义角色 | `--background`、`--accent` |
| component | 组件内部变量 | `--button-bg` |

### 字号标准

规定字号档位怎么命名，名字说用途不说数值，改字号不动名字。

角色取 Material 3 的五个，各分 large / medium / small：display、headline、title、body、label。完整角色映射尚未确定；首页实际字体与字号见 [DESIGN.md · Typography](../../../DESIGN.md#typography)。

### 动效标准

规定全站动效的时长与缓动，让节奏统一，降级时改一处即可。

采用 [transitions.dev](https://transitions.dev/skill) 的令牌刻度，不自行定数值。五个维度（时长、缓动、位移、缩放、模糊）的令牌与取值规则见 [DESIGN.md · 动效](../../../DESIGN.md#动效)，本文不另存一份。按 DTCG 的令牌类型，其中时长与缓动两类进令牌文件；位移、缩放、模糊随过渡一起调。

首页背景动画参数见 [DESIGN.md · 动态背景](../../../DESIGN.md#动态背景)，不属于交互动效。

**授权待确认**：transitions.dev 的 GitHub 仓库未声明许可证，站点区分免费与 Pro。skill 可安装使用；若把 `_root.css` 或过渡片段复制进发行包，需先与站点条款核对。

### 兼容版降级

现代版直接用新语法，兼容版在构建时改写成旧内核能认的形式。具体内核版本见 [浏览器支持](browser-support.md)。

| 对象 | 做法 |
|---|---|
| `oklch()` | 转 sRGB |
| `color-mix()` | 预计算成固定值 |
| `@layer` | 展开 |
| `:is()` `:where()` | 改写 |
| Tailwind 3.4 preflight | 补三条基础规则：`abbr`、`button` 与 `input`、`[hidden]` |
| JS 语法 | Vite `build.target` 按目标内核配置 |
| JS 内置 API | 检查项目与依赖实际用到的 API，按需改写或补 polyfill；构建不会自动补齐 |
| 第三方组件 | 引入兼容检查工具辅助，以实际版本和最终产物为准 |
| 实际运行 | 在目标内核验证页面加载与关键交互，未验证前标为目标支持 |


## 配色系统

采用 HeroUI v3 的语义角色、[Radix Colors](https://www.radix-ui.com/colors)（MIT）的 12 步用途组织方式和自有 OKLCH 色值。浅深两套独立生成；首页 Blue hour 背景与例外用色独立于通用色阶。

规范集中在 [DESIGN.md · Colors](../../../DESIGN.md#colors)：用途划分、中性与彩色色阶、首页实际用色、动态背景参数及对比度边界。本文件不另存一套色值。取色的权威画布是 [cst-pilot-colors.pen](../../../web/design/cst-pilot-colors.pen)，完整文件清单见 [DESIGN.md · 文件清单](../../../DESIGN.md#文件清单)。

## 版式

首页中文使用思源黑体，产品名、模型名和页脚使用 Inter，聊天页的行内工具名使用 JetBrains Mono。色卡说明文字使用的 Noto Sans SC 不纳入产品字体体系。

实际字号、字重和未定事项见 [DESIGN.md · Typography](../../../DESIGN.md#typography)。完整全站字号阶梯、行高、字距及字体加载策略尚未确定。
