# 登录页 AIR 背景

`cst-pilot-web.pen` 的「登录页 · 浅色」与「登录页 · 深色」使用 `blue-hour-air.glsl`。只用于 Pen 设计稿，不代表网页端已实现。

| 文件 | 用途 |
|---|---|
| `blue-hour-air.glsl` | WebGL 1.0 着色器，`@time` 驱动动画，两版共用 |
| `blue-hour-palette.png` | 浅色版色表，与浅色首页共用 |
| `blue-hour-air-dark-palette.png` | 深色版色表，分布对齐浅色版，底色由 `#22265E` 压深为 `#161C4A` |
| `make-blue-hour-air-dark-palette.mjs` | 深色色表生成器，运行 `node web/design/asset/make-blue-hour-air-dark-palette.mjs` 重建 |
| `login-preview/HRlE2.png` | 浅色场景预览用的浅色主页快照 |
| `login-preview/g1Rbpz.png` | 深色场景预览用的深色主页快照 |

## 构图与配色

参考 [FeralUI AIR / Wisteria haze](https://feralui.dev/gradients?g=2.q1ZKV7KqViouyS8oVrKKVlJ2dXQzcXNW0lFSdjKycHN2BbHcTJyMnY1ALEcDFwtXN6VYHaWSyoJUJSslR88gJR2lbCUrpRfTl7ycM09JR6lIyUrJLdTL090xONTXU0lHCaQuPLO4JLUoM1EhI7EqVam2FgA) 的四角色块布局、椭圆透明衰减与柔化。颜色取自 `cst-pilot-colors.pen` 的「cst-pilot 首页背景」，不使用原参考的粉、紫、青配色。

| 区域 | 色值 | 初始中心，左上为原点 |
|---|---|---|
| 底色与右上 | `#22265E`（深色版 `#161C4A`） | 78% / 26% |
| 左上 | `#3B4EA8` | 24% / 16% |
| 右下 | `#7785DE` | 88% / 84% |
| 底部过渡 | `#BCC4F1` | 50% / 93% |
| 左下 | `#E4E9FA` | 16% / 82% |

在椭圆半径的 22% 内保持完整权重，22%–72% 之间逐渐衰减。按图层顺序混合五个色标位置（0.10 / 0.30 / 0.50 / 0.70 / 0.90），对这个数值场做九点柔化，最后查询与首页相同的色表；色表在 Oklab 空间插值生成。浅色版查 `blue-hour-palette.png`，深色版查 `blue-hour-air-dark-palette.png`。

不直接混合深靛与冰蓝的 RGB，交界必须经过中间蓝色与蓝紫色，避免形成灰色过渡。保留深色上半区，让原有白色欢迎语可读；第五种颜色用于底部过渡，不覆盖标题。

## 动画与控制

| 参数 | 默认值 | 用途 |
|---|---|---|
| `u_speed` | 30 | 动画速度；设为 0 停止 |
| `u_phase` | 0 | 算法时间偏移；暂停时选择画面 |
| `u_drift` | 1 | 色块漂移幅度；设为 0 固定位置 |
| `u_softness` | 24 | 柔化尺度，按画布最长边缩放 |
| `u_grain` | 0.003 | 低强度静态 overlay 颗粒，不随时间闪动 |
| `u_palette` | `asset/blue-hour-palette.png` | 浅色版五色色表；深色版换成 `asset/blue-hour-air-dark-palette.png`，其余参数不变 |

`@time` 驱动各色块以不同相位缓慢漂移，默认水平方向周期约 105 秒。保留主页的可播放 Shader 形式，不复用主页 PRISM2 板条算法；链接本身未保存动画速度，漂移是本稿新增的动效设计。

Pen 不自动读取系统的减少动效偏好，需手动将速度设为 0。后续网页实现需处理减少动效偏好、后台暂停与不支持 WebGL 时的静态回退。

## 居中面板

登录组件保持 1080×1080，增加 32px 圆角、1.5px 内描边和柔和投影。背景与内容一同裁切到圆角内，面板本身不模糊。

| 项 | 设定 |
|---|---|
| 描边 | 顶部半透明白色，底部半透明蓝色 |
| 主投影 | 向下 24px，模糊 64px，扩展 -12px，`#11173333` |
| 近层投影 | 向下 4px，模糊 12px，`#11173314` |
| 场景预览 | 「登录页 · 浅色 · 呈现效果」与「登录页 · 深色 · 呈现效果」，均 1920×1080 |
| 预览面板 | 按 80% 等比展示为 864×864，x=528、y=108 |
| 背后主页 | 高斯模糊 20px，叠加 `#060B0E26` 遮罩 |
| 面板配色 | 卡片、输入框、方式切换与登录按钮取值见 [DESIGN.md · 登录页](../../../DESIGN.md#页面) |

场景预览使用 `login-preview/HRlE2.png`（浅色主页快照）与 `login-preview/g1Rbpz.png`（深色主页快照）；登录面板仍使用动态 Shader。快照只用于设计场景，正式网页应模糊真实主页，不加载这两张图片。预览面板为独立副本，不自动同步源组件。

## 范围与来源

登录页沿用当前背景与表单，圆角、边框和投影只作用于外层面板。Shader 从画布高度 70% 至 93% 将色表坐标平滑过渡至冰蓝色标，保证灰色页脚可读；独立浅色蒙层停用，不使用全屏背景模糊。保留当前 1080×1080 画布及其表单、文字和位置。两张主页和色卡不修改。

参考站点公开分发的 `JapaneseGradients-BRFKYYZf.js` 中 AIR 的色块位置、椭圆半径与透明衰减。当前 Shader 为独立实现；数值场九点柔化不等同于原站高斯模糊，采用首页色表映射，并增加底色、第五色块和漂移动画，不宣称逐像素复刻。原站演示的授权边界见 [Blue hour 背景说明](blue-hour.md#授权范围)。
