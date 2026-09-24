# 排版参考

同类 AI 对话产品的界面截图。用于确定 cst-pilot 主页面布局，不作为配色来源（配色见 `cst-pilot-colors.pen`）。

## 清单

| 文件 | 产品 | 主题 | 场景 |
|---|---|---|---|
| `01-qwen-studio.png` | Qwen Studio | 深色 | 空状态，无侧栏 |
| `02-zcode.png` | ZCode | 浅色 | 空状态，侧栏为项目树 |
| `03-codex.png` | Codex | 浅色 | 空状态，侧栏为项目树 + 最近 |
| `04-chatgpt-settings.png` | ChatGPT | 浅色 | 设置弹窗 |
| `05-chatgpt.png` | ChatGPT | 浅色 | 空状态 |
| `06-gemini.png` | Gemini | 浅色 + 蓝渐变 | 空状态，主区带蓝色渐变底 |
| `07-kimi.png` | Kimi | 浅色 | 空状态，侧栏分组多 |

## 共性

七张里六张是浅色。结构高度一致：

| 区块 | 宽度 | 内容 |
|---|---|---|
| 左侧栏 | 200 ~ 360px | 新建对话、搜索、分组列表（项目 / 最近）、底部账户 |
| 主区 | 其余 | 居中的标题或 Logo |
| 输入框 | 居中，宽约 900 ~ 1180px | 胶囊形或大圆角卡片，位于标题正下方 |
| 快捷入口 | 输入框下方 | 一排胶囊，四到七个 |
| 底部 | 侧栏底部 | 头像、用户名、套餐标签、设置 |

## 差异点

| 项 | 选项 | 出现 |
|---|---|---|
| 输入框形状 | 胶囊（全圆角） | Qwen、ChatGPT、Gemini |
| | 圆角卡片 | ZCode、Codex、Kimi |
| 标题 | 大标题文字 | Qwen、ChatGPT、Gemini |
| | 大 Logo 字标 | ZCode、Codex、Kimi |
| 主区背景 | 纯白 / 浅灰 | 多数 |
| | 蓝色渐变 | Gemini |
| 输入框上沿 | 上下文标签行（项目、分支） | ZCode、Codex |
| 快捷入口位置 | 输入框下方 | ZCode、ChatGPT、Kimi |
| | 无 | Qwen、Codex、Gemini |
| 侧栏分组 | 单一列表 | ChatGPT、Gemini |
| | 项目树 + 最近 | ZCode、Codex |
| | 多组功能菜单 | Kimi |

## 对 cst-pilot 的取舍

页面结构以[界面画布](../../cst-pilot-web.pen)为准，跨页面视觉规则见 [DESIGN.md](../../../../../DESIGN.md)。
