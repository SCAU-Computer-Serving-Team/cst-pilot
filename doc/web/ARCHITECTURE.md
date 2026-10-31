# Web 前后端架构

本页说明 Web 前后端目录组织、流式渲染与状态同步、草稿图片存储回收、分支树几何算法及工具卡片的数据转换管线。行为要求见 [SPEC.md](SPEC.md)，接口定义见 [API.md](API.md)。

## 工程边界与静态服务

前端源码位于 `src/web/frontend/`（包名 `@cst-pilot/web`，React 19 + Vite + Tailwind v4 + react-router 框架模式 `ssr: false`），构建产物输出至 `agent/home/extensions/web/static/`。后端位于 `agent/home/extensions/web/server/`。

```text
src/web/frontend/app/
├── routes/         路由页面装配（主页、全部会话、工作台、树、设置、登录、账号）
├── cards/          开放工具的结果转换（map-tool.ts / special-tool-data.ts）与渲染组件
├── conversation/   对话流、过程分段、Markdown 安全切分、排队面板与扩展提问
├── composer/       输入框行内标记编辑器、补全面板、草稿与 IndexedDB 图片引用、上下文额度
├── images/         消息、工具与草稿共用的图片预览入口与原位缩放
├── tree/           分支树可见重挂、几何列计算（branch-tree.ts）与分支总结参数编解码
├── settings/       设置资源并发缓存、高度过渡钩子、模型范围排序与自定义服务表单
├── auth/ & account/ OAuth 弹窗隔离、多步授权面板与 CSTOA 账号资料展示
├── shell/          最近会话、共享列表状态、乐观批量删除、全局通知栈、断线提示、Blue hour / AIR WebGL 背景
└── data/           HTTP/SSE 客户端、24ms 流式播放器、连接健康检查与错误格式化
```

依赖方向固定为 `routes -> 功能域 -> data`；浏览器代码严禁引入 Node 原生模块或直接读取凭据文件。

后端 API 按职责拆分；同源检查与写入去重由入口统一执行。

| 文件 | 职责 |
|---|---|
| `server/api.ts` | 同源检查、生命周期、会话操作、写入串行化与去重；访问日志归当前实例所有 |
| `server/api/models.ts` | 模型清单、选择、思考强度与启用范围 |
| `server/api/auth.ts` | 登录、凭据与自定义服务配置，管理授权流程的释放 |
| `server/api/http.ts` | JSON 正文校验、字节上限与响应格式 |
| `server/api/errors.ts` | 将业务错误和任意抛出值映射为统一错误响应 |

静态服务（`web/server/http.ts`）路由规则：

- `/api/*` 交由 API 路由处理，未命中返回结构化 404 JSON。
- 命中 `static/` 下真实存在的文件时按 MIME 类型返回；带内容哈希的 `/assets/*` 资源配长缓存，`index.html` 每次重验证。
- 未命中文件但 `Accept` 包含 `text/html`、路径不属于资源目录且无文件扩展名时，返回 `index.html` 供前端路由解析（并按主题 Cookie 写入 `data-theme`）。

## 流式渲染与 SSE 状态同步

1. **Markdown 连续更新（`markdown.tsx`）**：流式与完成态共用同一组件、容器和文本包装结构，按块序号更新；追加段落不移动已读段落。高亮只在定稿时处理代码块，逐词节点按词序保持身份；历史正文不播放逐词动画。块级代码与原文共用 `shell/code-block.tsx`，复制时读取正文节点的当前 `textContent`，排除按钮与反馈；复制状态按块隔离，卸载时清理反馈计时器。
2. **平滑步进播放（`stream-player.ts`）**：按 24ms 步进消费接收缓冲，每步输出 1–12 个字符（随积压动态加速）并吸附词边界，消息结束时立即刷出全部剩余文本。
3. **SSE 背压与断流恢复**：服务端对单连接未发送积压允许至多 8 MiB 缓冲（容纳大体积思考或工具结果），每 20 秒发送一次 `heartbeat` 事件，心跳不占用业务回放序号；前端连续 45 秒未收到业务帧或心跳时判定断流。服务端在进程内保留最近 256 条业务事件，重连携带 `Last-Event-ID`；序号超出保留窗口或进程重启时收到 `reset` 信号并全量重取会话快照。
4. **结束事件与快照衔接（`session-stream.ts`）**：`message_end` 直接合并完整的用户、助手和工具结果消息，助手结束时清除该消息的播放态；新一轮用户消息先进入展示，快速回复不会短暂归入上一轮。延迟快照不得覆盖更新。轮次、过程与工具组件按消息时间戳保持身份，HTTP 返回持久化条目 ID 时不重新挂载。连接重置保留正文，等待新快照接替；滚动跟随在绘制前完成。
5. **轮次耗时持久化**：助手回复完成时，后端向会话写入 `cst-web-work-duration` 自定义条目（不进入模型上下文），在 HTTP 快照的 `message.work` 中返回精确起止耗时，保证刷新或重开后过程头耗时不变。

## 会话导出与产物保存

| 模块 | 职责 |
|---|---|
| `shared/conversation.ts`、`shared/exports.ts` | 前后端共用消息、导出选项、快照与任务契约；仅包含类型 |
| `server/exports/snapshot.ts` | 当前分支投影、参数校验、统一快照时间与归档日期、过程筛选与导出副本脱敏 |
| `server/exports/store.ts` | 有界串行队列、后台任务、文件原子登记、失败回收与完整性核验 |
| `server/exports/browser.ts`、`render.ts` | 启动隔离 Edge / Chrome，通过浏览器调试协议取得 HTML 或逐页 PNG；结束后释放进程和配置 |
| `server/api/exports.ts` | 同源 API，创建、预览、查询及文件读取；与入口写入去重共用 |
| `app/routes/export.tsx`、`export-render.tsx` | 导出工作区与独立只读文档；预览主题不改变工作区主题 |
| `app/exports/` | 共用文档呈现、离线 HTML 序列化、HeroUI 导出下拉与页面布局 |

创建任务先固定快照并写入内部登记，随后释放会话写锁，渲染过程不调用模型。临时页面通过任务令牌读取快照；令牌放在 URL 片段中，不进入访问日志，任务结束后失效。

登记与渲染临时文件位于 `agent/home/session-exports/`，最终产物位于 `artifacts/<日期>/sessions/`。全部页面生成后才发布文件与完成状态；失败回收已发布文件。文件名携带导出 ID，读取时校验目录真实路径与 SHA-256，分享使用同一份已登记产物。

PNG 以固定的 1240 × 3200 CSS 像素视口逐段滚动截图，保留两倍像素密度并启用快速无损编码。分页切点排序后单次遍历，每张图生成后立即写入暂存目录，内存中只保留当前图。

离线文档复用 Markdown、代码、工具块、特殊工具与聊天主题。仅内嵌文档已使用的字体，各资源并行读取并按 URL 去重；附图直接保留内嵌数据。动作控件和脚本移除，内容安全策略关闭脚本、表单及外部资源加载。

HTML 使用原生 `details` 保留折叠，不依赖现场服务。内嵌字体后的 HTML 通过浏览器调试协议按 128 Ki 字符分段读取，避免大响应中断连接。

## 会话预载与缓存

`WorkspaceLayout` 挂载唯一的 `SessionsProvider`，共用列表请求、全局事件订阅与乐观删除状态。侧栏显示排序后的前 20 项，`/sessions` 按本地自然日分四组显示完整列表；`session-list.ts` 共用排序与互斥分组逻辑，页面在本地下一个零点重新分组，缺失或无效时间归入“更早”；`SessionEntry` 共用会话行，`useSessionActions` 与 `SessionActionsOverlay` 共用菜单、重命名、删除确认及失败恢复。管理模式使用原生复选框，焦点回到对应会话或管理入口。

侧栏取得会话列表后，用 React Router 的 `PrefetchPageLinks` 预载共用的聊天路由入口及其依赖，只为一个会话插入预载声明。特殊工具样式并入全局样式，首次进入聊天无需再请求样式文件。

会话行按下时仅淡化行内内容，选中背景由单个滑动选区绘制。点击处理时启动选区移动，选区跟随待提交路径，导航结束或取消后按路由状态校正；修饰键点击和取消的点击不改变当前选区。尺寸变化但目标位置未变时保留正在播放的滑动。

正文保留淡入与线性颜色动画。首次读取的历史 Markdown 不创建逐字节点；流式正文及其定稿保留逐字节点。历史折叠过程首次展开时挂载内容，运行中立即挂载；挂载后收起保留节点、工具展开状态和排行分页。

`session-cache.ts` 维护当前标签页的会话详情内存缓存，上限 20 项，按最近使用淘汰。侧栏首次取得会话列表后，按更新时间选取滚动七天内最新 10 项，以两个并发请求预载；预载与打开会话共用在途请求，单项失败不阻断其余项。

聊天页同步读取命中项，并在后台重新获取快照与连接 SSE。列表与删除事务清除已删除项，版本标记阻止旧响应覆盖事件更新；被事件作废的在途响应不能代替下一次强制刷新。缓存不写入 `localStorage`、`sessionStorage` 或 `IndexedDB`。

## 模型选择与凭据提示

`server/api/models.ts` 分开提供会话选择与默认模型保存，模型清单同时返回目录、默认值和凭据就绪状态。服务端在创建会话与接受消息前预检凭据，拒绝时不写入收件箱或附件。

前端 `composer/model-picker.tsx` 共用合并入口、双行主菜单、模型列表与思考强度列表。弹层固定宽 224px，按行数计算目标高度，以 transitions.dev 的 card resize 连续调整。主菜单与列表保留为两个页面，同时使用 page side-by-side 左右滑动与淡化过渡；隐藏页设为 inert 并移出辅助技术读取范围，保留上次列表内容供返回退场。列表超高时内部滚动，减少动态时立即切换。选择成功或列表内按 Esc 返回主菜单并恢复对应行焦点；点击外部直接关闭，主菜单内按 Esc 关闭。提交期间禁止重复写入，失败保留列表与已确认值，过期响应不改变当前视图。首页只保留本次模型和强度，创建时一并传入；会话内调用思考强度接口。后端会话档位取 Pi SDK，首页按模型档位映射与默认设置计算。

前端 `data/model-selection.ts` 保留用户选择并只刷新其状态；`settings/default-model-picker.tsx` 提供可搜索的独立默认选择器；`auth/model-key-dialog.tsx` 使用原生对话框提醒配置。配置成功只返回受限的本机来源路由，草稿由原有存储恢复，不自动发送。

## 设置分类导航

`routes/settings.tsx` 按四分类的菜单序号判定滑动方向，服务配置子页归入“模型服务”。路径变化时同步记录方向，分类内部的子页导航保留独立入退场，资源预载沿用 `SettingsLink`。

分类正文使用 `settings-page` 原生视图快照，按 transitions.dev 的 page-side-by-side 规则执行 250ms 横向位移与淡化。选中下划线使用独立的共享快照插值位置与宽度；跨出设置区时并入整页快照。不支持原生快照时新内容横向入场，减少动态时直接切换。

## 运行中光影

`running-light.tsx` 按字素分割状态文字，每个字位使用浏览器动画改变颜色混合强度；`running-light-timing.ts` 生成中心、相邻、隔一字的三档权重。浅色向白色混合，深色向暗色混合；周期为 2000ms，状态数字变化时保留当前进度。

特殊组件使用按整体尺寸归一化的 SVG 线性遮罩，形成约占 40% 面积的右下角柔边三角形；反光层采用 `soft-light` 混合，越向内越淡。交叉观察器与页面可见性事件暂停离屏和隐藏页动效，减少动态时取消动画并恢复原色；反光层不接收点击。

## 图片预览

`images/image-preview.tsx` 提供 `PreviewableImage`，供消息附图、工具图片与草稿缩略图复用。点击时才挂载原生模态 `dialog`，通过 Portal 放到页面根部，避开气泡和工具卡片的布局及样式限制。

预览复用已加载图片，以原图与居中大图的位置及尺寸差执行 Web Animations 缩放，背景同步淡入淡出。关闭从当前呈现值收回；窗口变化时关闭，减少动态偏好下直接呈现或收起。组件卸载时取消动画和监听，恢复原图可见性及页面滚动；样式并入全局样式，打开预览无需补载 CSS。

## 草稿图片存储与跨页回收

未发送草稿保存在当前标签页 `sessionStorage`；未发送图片的原始字节按 SHA-256 存入浏览器 `IndexedDB`：

- `localStorage` 仅发布各标签页持有的图片哈希引用表与心跳时间戳。
- 配合浏览器 `Web Locks API` 存活锁与 60 秒页面刷新宽限期：仅当图片未被任何存活标签页、正在进行的异步读取或宽限期内的刷新页引用时，才在删除、发送成功或启动检查时清理 `IndexedDB` 孤儿字节。
- 消息被后端 `POST /api/sessions/:id/messages` 接受后，图片字节按 SHA-256 写入服务端会话附件目录，后续通过 `/api/sessions/:id/attachments/:attachmentId` 读取，删除会话时同步清理附件目录。

## 分支树几何算法

`app/tree/branch-tree.ts` 将后端 `getTree()` 转换为与 Pi TUI `TreeList` 几何语义完全一致的展示行（由 `tree-parity.test.ts` 与真实 Pi 对照验证）：

1. 在原始树上按“当前叶所在路径优先、其余兄弟按时间”执行深度优先遍历，固定每条目的 `activePath` 与 `traversalOrder`。
2. 应用五档过滤，将隐藏条目的可见子条目重挂到最近的可见祖先，保持原始 `traversalOrder` 不重排。
3. 计算每行几何属性：根节点 `indent = 0`；真实分叉的子条目 `indent + 1` 且标记 `showConnector = true`（绘制方框，位于 `connectorColumn = indent - 1`）；段首的第一代后代 `indent + 1`；非末位兄弟的子树在对应列延续竖线 `gutters`。有后代的连接符行在槽位后额外预留 `32px`，使分支起始行图标与后续单链内容对齐。

## 工具分类与过程计数

`app/cards/tool-kind.ts` 统一判断特殊组件与独立展示工具，供内容分段、工具分组与过程计数共用。`sys overview`（含缺省 scope）、`web_search`、命令方案与交付，以及 BitLocker 解锁／解密独立呈现；BitLocker 状态查询和其他 sys 查询保持普通工具，读取图片保留独立图片卡片。

`turn-segments.ts` 根据普通调用的实际结果生成后续过程标题。思考、正文和图片保留内容入口，空段省略；首段工作时长入口保持稳定，流式结束和刷新不会因计数变化而移除。

## 工具卡片转换

### 通用骨架与 Block 联合类型

`app/cards/map-tool.ts` 将工具原始返回（优先读未裁剪的 `details`，缺失时读 `content`）转换为统一的 `CardView`，交由 `tool-card.tsx`（基于 HeroUI `Card`、`Disclosure`、`Alert`、`Button`）渲染：

```ts
type Block =
  | { kind: "fields"; id: string; title?: string; rows: { label: string; value: string }[] }
  | { kind: "listing"; id: string; items: { name: string; metrics: { label: string; value: string }[] }[] }
  | { kind: "text"; id: string; text: string }
  | { kind: "image"; id: string; mimeType: string; data: string }
  | { kind: "notice"; id: string; text: string; status: "default" | "warning" | "danger" }
  | { kind: "commands"; id: string; items: { shell: string; summary: string; command: string; admin: boolean }[] };
```

### 工具主模板与字段来源

整次 `isError: true` 统一使用错误**原文块**；正常或降级返回按下表映射：

| 工具 | 渲染形态／主模板 | 核心字段来源与转换规则 |
|---|---|---|
| `disk` | 概览组（`space/info/health/all`）<br>排行组（`usage`） | `space[]`、`volumes[]`、`physicalDisks[]`、`smart[]` 每项生成字段组；`usage` 将 `topDirs`、`topFiles`、`extAgg`、`staleFiles` 生成排行组；保留 `infoNotice`、`smartErrors`、`degradedFrom` |
| `ls` | 排行组 | `path/totalChildren/totalSize/method` 生成目录概况字段组；`entries[]` 生成大小排行组；`omitted` 生成截断提示；任一子项不完整时 `pct` 保持为空 |
| `sys` | 特殊组件（`overview`）<br>排行组（`proc/gpu/io`）<br>概览组（`sensor`） | `overview` 走内嵌组件（内存 MB 转 GB）；`proc` 生成 CPU／内存双排行；`gpu` 生成进程排行 + 适配器／NVIDIA／LHM 字段组；`io` 生成磁盘与进程排行；`sensor` 生成硬件读数、热区与频率字段组 |
| `startup` | 概览组 | 依次生成自启概况、`regItems[]`（`disabled` 映射为已禁用／未禁用／状态未知）、`startupFolders[]`、`services[]` 字段组 |
| `eventlog` | 概览组（列表类）<br>原文块（`detail`） | 列表类将 `events[]` 生成事件字段组，`counts[]` 生成来源排行组，附范围统计与 `security` 权限降级提示；`detail` 生成事件头字段组 + `msg` 原文块 |
| `driver` | 概览组 | `problem/find` 生成匹配设备与汇总字段组；`core` 按网卡、蓝牙、音频、显示、服务、驱动分组；`external` 按外设与可移动存储分组 |
| `command_plan` / `runbook_deliver` / `runbook` | 特殊组件 | 标题、命令、风险档与版本优先取 `details.runbook`；缺失 `details` 时读取结果 JSON；缺少结构化命令时回退调用参数。方案与导出共用卡片，只有交付成功显示文件链接 |
| `bitlocker` | 概览组（`status`）<br>特殊组件（`unlock/decrypt`） | `status` 按卷生成状态字段组；`unlock/decrypt` 走特殊组件，合并 `details.bitlocker.volumes[]`、每卷 `operation` 与会话待处理交互请求 |
| `web_search` | 特殊组件 | 行内展示检索状态，展开调用参数 `queries / query`，状态比对 `successfulQueries` 与 `queryCount` |
| `source_check` | 概览组 | 依次展示主张判断（`claims[]`）、来源排行（`sources[]`）、引文原文块（`passages[]`）与续读标识 |
| `fetch_content` | 原文块 | 抓取概况字段组（`urls/successful/mode/totalChars`）+ `content` 文本原文块或图片组 + 截断续读提示 |
| `get_search_content` | 原文块 | 按搜索、研究（JSON 片段）或网页抓取来源生成元数据字段组 + `content` 原文块 |
| `fffind` / `ffgrep` | 原文块 | `details.totalMatched/totalFiles` 统计字段组 + `content[].text` 路径或代码匹配原文块（不将 `totalFiles` 误作命中数） |
| `read` | 原文块 / 图片例外 | 文本读取展示绝对路径字段组 + 正文原文块；图片读取优先展示图片块，再展示绝对路径，忽略 `Read image file [...]` 标注文本 |

### 特殊工具交互

- **BitLocker 本地输入与确认**：Web 后端在内存中维护绑定 `sessionId + entryId + toolCallId + requestId` 的待处理请求（3 分钟有效期，带递增版本），随会话快照与 SSE 下发。前端 `bitlocker-input.tsx` 通过 `POST /api/sessions/:id/bitlocker/:requestId` 一次性提交恢复密码或确认解密，绕过持久化幂等日志，确保恢复密码永不落盘。
- **Runbook 文件打开**：点击卡片文件名调用 `POST /api/sessions/:id/runbook/open`，后端按助手条目和调用 ID 匹配 `runbook` 或 `runbook_deliver` 的结果，从结果读取路径，校验文件属于 `artifacts/` 后启动系统记事本，并对重复点击做去重保护。
