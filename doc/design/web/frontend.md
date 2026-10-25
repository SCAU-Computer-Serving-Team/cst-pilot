# Web 前端实现

本页定义源码、依赖、加载和构建。行为见[前端规格](../../web/SPEC/frontend.md)，视觉规则见 [DESIGN.md](../../../DESIGN.md)。

## 页面形态

| 项 | 实现 |
|---|---|
| 框架 | React 19、TypeScript、HeroUI v3 |
| 构建 | Vite、Tailwind v4、react-router 框架模式，`ssr: false` |
| 页面 | 静态 SPA，由扩展 HTTP 服务提供 |
| 运行服务器 | Pi 进程中的一个 HTTP 服务，不增加转发层或运行时进程 |

兼容版列入后续迭代，计划共用组件和地址，使用 Tailwind 3.4 与独立样式构建；不直接复用 HeroUI v3 产物。

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

## 正文排版

Markdown 的数值与相邻规则集中在 [DESIGN.md](../../../DESIGN.md#markdown-块间距)。`app/styles.css` 的 `.markdown` 作用域承载正文、六级标题、列表、引用、代码与表格；不把读取间距写入全站控件变量。

正文容器铺满所在内容列，用单侧段前边距控制块间距。列表项内的嵌套列表与多段正文分别处理；代码清除继承的字距。浏览器测试 `markdown-rendering.e2e.ts` 检查标题、列表、引用、代码和表格，以及流式完成后的正文完整性、浅深主题、窄幅与放大。字号、字距和块间距按设计变更人工验收。

## 状态与加载

- 会话首屏先取启动快照，再按页面取数；独立请求并行。
- 账号页、设置和菜单共用 `routes/layout.tsx` 中的一份 `AccountProvider`，避免重复读取资料。
- 上下文面板打开时按当前服务查询额度。切换服务或收到认证、额度事件时清除旧值，请求序号阻止迟到响应覆盖新状态。
- 会话结束或树导航发布带 `sessionId` 的 `usage_changed`，输入框仅为当前会话重读缓存统计；不按流式增量请求，切页取消旧请求。
- 助手消息结束时，服务端用 `cst-web-work-duration` 自定义会话条目保存用户消息起点与真实完成时刻。HTTP 快照增加 `message.work`，不改变模型上下文；前端沿用持久化区间展示耗时。

### 设置页

`routes/settings.tsx` 承载共用标题栏、设置内导航、内容列与保存反馈；子路由按需取数。`settings/settings.css` 限定在设置界面，账号页和登录页继续使用各自样式。

| 模块 | 职责 |
|---|---|
| `settings/use-settings-resource.ts` | 资源读取、串行写入、失败回滚与共享事件刷新；读请求可取消，请求序号阻止迟到响应覆盖写入 |
| `settings/provider-list.ts`、`auth/provider-services.tsx` | 已配置与可添加服务分类、登录状态和单一行操作 |
| `settings/provider-configuration.tsx` | 密钥完成输入后启动 Pi 授权流程，高级地址和移除凭据确认；已有密钥不读取 |
| `settings/model-scope.ts` | 模型范围归一、勾选与双向排序；沿用服务端默认范围语义 |
| `settings/settings-frame.tsx` | 分区、返回、加载错误与保存通知；连续保存更新同一条通知的计时 |

主题与模型范围按资源分别串行写入，失败恢复已确认值。OAuth、多步骤 API KEY 与登录页共用 `OAuthPanel` 和流程版本校验；切页或切换方式取消未完成流程。

### 会话管理与通知

| 模块 | 职责 |
|---|---|
| `shell/session-groups.ts` | 按本机日历日期分组，计算未选、全选和半选状态 |
| `shell/session-management.tsx` | 保存选择、确认与结果；原生删除对话框默认聚焦取消 |
| `shell/session-deletion.ts` | 逐项调用 DELETE，一项失败不阻止后续项；不自动重试 |
| `data/optimistic-session-deletion.ts` | 隐藏待删除项，过滤迟到列表，失败时恢复最新快照和排序 |
| `shell/notification.tsx` | 页面布局外的唯一通知区域；共用注册表保持退场节点，以侧栏之外的主内容区域为居中基准 |
| `shell/error-notice.tsx`、`data/error-presenter.ts` | 行内错误与默认收起的技术详情；文案区分已接受消息和待确认提交 |

会话行、文字与勾选框持续挂载，进退只过渡位移和透明度。管理操作位于新对话右侧，不增加列表高度；右键菜单支持方向键。删除当前会话的回滚检查导航键，保留用户后续导航。

通知共用样式和堆叠区域，各功能分别维护生命周期。用户行为见[会话管理规格](../../web/SPEC/frontend.md#会话管理)，尺寸与动效见[视觉规范](../../../DESIGN.md#弹出通知)。

### 消息与共享配置

服务端缓存与凭据复核见[运行设计](runtime.md#请求缓存)。

- 每场会话一条 SSE，增量按 `toolCallId` 和消息 ID 关联；刷新先取快照，再恢复订阅。
- 全局流处理会话列表、设置、凭据与 OAuth 状态。共享页面滚动和 Esc 监听由 `shell/panel-dismiss.ts` 管理；内部滚动不关闭面板。
- sessionStorage 保存标签页草稿；跨页图片引用与回收见[运行设计](runtime.md#图片与草稿)。
- Web 主题写入 `agent/home/web-settings.json`，TUI 主题仍由 Pi `settings.json` 管理；页面取自己的外观配置。文件补全按当前会话 ID 请求，前端缓存随会话隔离，后端使用历史 cwd。
- 服务器按主题 cookie 给 `<html>` 写 `data-theme`，水合允许该属性差异；不插入会改变树结构的内联主题脚本。
- 主页与工作台双向切换使用独立 View Transitions 快照：背景向上 slide 退出、从上方滑入，主页退场用 ease-in-out，工作区进入与返回主页用 smooth-out。主页背景仅在主页挂载，退出由浏览器旧快照承载；没有 WebGL 时使用相同的整幅 CSS 背景快照，局部模糊保持一致。时长、延迟与位移取值见 [DESIGN.md 的页面衔接与绘制](../../../DESIGN.md#页面衔接与绘制)。
- 会话快照命名挂在持续挂载的 `chat-message-surface` 外层。加载提示与会话正文在其内部替换，不替换快照所属元素，避免数据加载时取消正在播放的页面过渡。
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
| `npm run test:web:shared-audit --prefix agent` | 独立 Pi 进程与共享文件、工具、写权、上下文、配置及主题边界 |
| `npm run test:web:e2e --prefix agent` | 构建后运行隔离 Chrome、真实 HTTP 和 Pi 新进程；覆盖持久化、故障恢复及重启 |

自动检查、真实供应商授权和目标环境验证分别执行，方法见[Web 测试](../../test/web-e2e.md)。MVP 已交付范围见[阶段说明](../../web/MVP.md)，后续验证任务见[待办清单](../../Todo.md)。
