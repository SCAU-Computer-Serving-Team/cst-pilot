# Web 前端实现

本页定义源码、依赖、加载和构建。行为见[前端规格](../../web/SPEC/frontend.md)，视觉规则见 [DESIGN.md](../../../DESIGN.md)。

## 页面形态

| 项 | 实现 |
|---|---|
| 框架 | React 19、TypeScript、HeroUI v3 |
| 构建 | Vite、Tailwind v4、react-router 框架模式，`ssr: false` |
| 页面 | 静态 SPA，由扩展 HTTP 服务提供 |
| 运行服务器 | Pi 进程中的一个 HTTP 服务，不增加转发层或运行时进程 |

兼容版在 MVP 后实现，计划共用组件和地址，使用 Tailwind 3.4 与独立样式构建；不直接复用 HeroUI v3 产物。

## 工程边界

| 路径 | 职责与分发 |
|---|---|
| `src/web/frontend/` | 包标识 `@cst-pilot/web`，源码与开发依赖，不进入发行包 |
| `app/routes/` | 页面装配与接线 |
| `app/cards/` | 工具字段映射与卡片 |
| `app/conversation/` | 会话流、Markdown、队列与提问 |
| `app/composer/` | 编辑器、补全、图片与上下文 |
| `app/tree/` | 树结构、总结与派生 |
| `app/shell/` | 侧栏、主题、动态背景与面板 |
| `app/settings/`、`auth/`、`account/` | 设置、授权、账号功能 |
| `app/data/` | 接口、会话状态与流式节奏 |
| `agent/home/extensions/web/server/` | 后端接口与会话运行 |
| `agent/home/extensions/web/static/` | 构建产物，不入 Git，随发行包分发 |
| `agent/home/extensions/web/test/` | 后端测试，由发行脚本排除 |

域内不按组件、工具函数等技术类型再拆目录。依赖方向为 routes → 功能域 → data；共享模块不得引入 Node API、凭据读取或诊断执行代码到浏览器。

前端依赖不进入 `agent/home/npm/`。扩展入口只加载服务端依赖，开发配置放前端目录。新增仪表盘建立独立功能域。

## 状态与加载

- 会话首屏先取启动快照，再按页面取数；独立请求并行。
- 每场会话一条 SSE，增量按 `toolCallId` 和消息 ID 关联；刷新先取快照，再恢复订阅。
- 全局流处理会话列表、设置、凭据与 OAuth 状态。共享页面滚动和 Esc 监听由 `shell/panel-dismiss.ts` 管理；内部滚动不关闭面板。
- sessionStorage 保存标签页草稿；跨页图片引用与回收见[运行设计](runtime.md#图片与草稿)。
- 服务器按主题 cookie 给 `<html>` 写 `data-theme`，水合允许该属性差异；不插入会改变树结构的内联主题脚本。
- 会话行使用真实链接，内部不嵌按钮；避免浏览器默认整页跳转。
- 主页与工作台双向切换使用 View Transitions 独立快照。首页输入框与工作台输入区分别命名，在各自终点下方入退场；页脚采用相同的局部位移、淡入淡出与模糊。快照仅在涉及首页的路由过渡期间命名，工作台内部导航不重复首页动效。
- 侧栏保持固定宽度，主区布局仅更新一次，使用前后中心位置差执行位移；背景尺寸以局部缩放衔接，不拉伸文字。中途反向先采集可见位置，再取消旧动画；读取 CSS 时长需兼容 ms 与 s 单位。主页背景展开期间临时允许跨主区边界绘制，完成、取消或反向后清理状态并恢复裁切；外层布局继续限制视口范围。缺少快照能力时保留局部入场；减少动态偏好下跳过位移。
- Shader 由时间戳推进，隐藏页暂停；登录面板背后的主页保持静态，避免与 AIR 同时连续绘制，也不订阅模型变更；尺寸由 ResizeObserver 缓存，uniform 位置缓存，绘制像素预算 250 万。刷新率由浏览器与硬件决定，前端不设置实验性帧率开关。

## 静态服务与路由

| 路径 | 实现 |
|---|---|
| `/api/*` | 后端处理，未命中返回接口错误 |
| 已存在静态文件 | 限定 static 根目录及真实路径，按类型发出 |
| 未命中导航 | Accept 含 HTML、非资源目录且无扩展名时返回页面入口 |
| 缺资源、非法路径 | 返回失败，不发页面入口 |

页面路由表在浏览器构建产物中，新增页面通常只修改前端路由表。服务绑定 `127.0.0.1`，校验 Host；写请求要求同源 Origin 与自定义请求头，不开放跨站 CORS。

内容哈希资源使用长缓存；入口 HTML 和非哈希资源重验证。JSON 与静态短响应关闭连接，避免与 SSE 争抢每域连接数。回环传输不增加压缩或 Range。

## 打包接入

构建发布脚本把 `build/client` 同步到 static。发布前检查三档 WOFF2、字体许可、派生说明与字符集清单。发行资源校验由 `pack/release-checks.mjs` 承载，覆盖入口引用、字体、许可与 SHA256SUMS。

Python、FontTools 和开发依赖仅用于构建，不进入现场运行链；字体生成见[字体分发](fonts.md)。

## 检查

| 命令 | 范围 |
|---|---|
| `npm run lint --prefix src/web/frontend` | 从前端目录运行 Biome，识别 React 规则 |
| `npm run typecheck --prefix src/web/frontend` | 路由类型与 TypeScript |
| `npm run test --prefix src/web/frontend` | 功能纯函数 |
| `npm run build --prefix src/web/frontend` | 字体准备、构建与发布 |
| `npm run test:fonts --prefix src/web/frontend` | 分发字体与覆盖校验 |
| `npm run check --prefix agent` | 后端 Biome 与类型 |
| `npm run test:web --prefix agent` | Web 接口与运行测试 |
| `npm run test:web:e2e --prefix agent` | 构建后运行隔离 Chrome、真实 HTTP 和 Pi 新进程；覆盖持久化、故障恢复及重启 |

检查不等于真实供应商授权或最低浏览器版本验收。阶段与未完成验证由 MVP 管理。
