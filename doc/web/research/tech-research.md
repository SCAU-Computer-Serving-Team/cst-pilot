# Web 技术调研记录

状态：调研记录。当前决策见 [tech-decisions.md](tech-decisions.md)，浏览器基线见 [browser-support.md](browser-support.md)。
相关：[tech-decisions.md](tech-decisions.md)、[PRD](../../PRD.md) W1–W4、[Todo](../../Todo.md) 第 3 条

## 降级能力分层

| 层 | 降级路径 |
|---|---|
| JS 语法 | Vite `build.target` |
| JS 内置 API | plugin-legacy 注入 core-js，代价是双份产物与 SystemJS |
| CSS | Lightning CSS 能转颜色函数与嵌套；`:is()`、`@layer`、容器查询、`aspect-ratio` 只能避开 |
| Tailwind v4、shadcn 当前版 | 无 |
| Tailwind v3.4、React 18 | 可用 |

## pi 扩展能否被 TUI 与 Web 同时消费

不需要拆进程。同一个内核支持 `tui`/`rpc`/`json`/`print` 四种模式，TUI 只是其中一个消费者；扩展与它同进程，用 `pi.on` 订阅同一份事件流。输入侧同样是多源的：`input` 事件的 `source` 会区分 `interactive`、`rpc`、`extension`，`pi.sendUserMessage()` 发的是真实用户消息，会出现在 TUI 记录里。

## 兼容版实测

本机真跑构建加静态扫描，结论如下。基线与现场策略见 [browser-support.md](browser-support.md)。

| 项 | 结论 |
|---|---|
| Tailwind v4 | 硬不可用。产物含 `@layer`，86 会整块丢弃 |
| Tailwind 3.4 | 可用。需补三条 preflight 规则，见 [浏览器支持](browser-support.md) 兼容版降级 |
| React 18 与 19 | 生产构建都没用到 86 缺的内置方法 |
| Radix 当前版本 | 自己实现了 `at` / `toSorted` / `toReversed`，对 `Array.prototype.at` 有特性检测回退 |
| 未验证 | 真 Chromium 86 实跑；shadcn 按 Tailwind 版本发组件（未实测） |

## 事实与依据

| 主题 | 结论 |
|---|---|
| 会话数据 | 工具返回的 `details` 会写进 session 文件，刷新可回放 |
| 打包 | 发行树检查会扫 JSON 的键名，命中 `token`、`secret` 一类且值非空就报错；文件数直接影响打包耗时 |
| 组件生态 | shadcn 当前版要求 Tailwind v4（颜色从 HSL 改 OKLCH），官方称 v3 项目继续发 v3 组件；Recharts 无官方基线 |
