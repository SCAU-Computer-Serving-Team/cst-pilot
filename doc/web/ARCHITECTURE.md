# Web 前后端架构

本页说明 React Web 前端（`src/web/frontend/`）与 `cst-server` 导出渲染管线的目录结构，覆盖聚合 SSE 同步、流式渲染、客户端草稿存储及工具卡片映射架构。行为要求见 [SPEC.md](SPEC.md)，接口定义见 [API.md](API.md)，阶段适配细节见 [8.web.md](../rust/plan/8.web.md)。

## 前端工程边界与静态托管

前端源码位于 `src/web/frontend/`（包名 `@cst-pilot/web`，React 19 + Vite + Tailwind v4 + React Router `ssr: false`），构建产物输出至 `agent/home/extensions/web/static/`，由 `crates/cst-server/src/routes/static_files.rs` 托管。

```text
src/web/frontend/app/
├── routes/         路由页面装配（主页、全部会话、工作台、分支树、导出、设置、登录、账号）
├── cards/          17 个开放工具的结果转换（map-tool.ts / special-tool-data.ts）与卡片组件
├── conversation/   对话流、过程分段、流式 Markdown、排队面板与扩展提问
├── composer/       行内标记编辑器、补全面板、草稿与 IndexedDB 图片引用、上下文额度面板
├── exports/        聊天与导出共用的只读文档呈现、双 iframe 后台预览交接与保存结果栏
├── images/         消息、工具与草稿共用的原位缩放图片预览
├── tree/           分支树五档过滤重挂、几何列计算（branch-tree.ts）与总结参数编解码
├── settings/       设置资源并发缓存、分类横向快照转场、模型范围排序与自定义服务表单
├── auth/ & account/ OAuth 无 opener 弹窗隔离、设备码二维码面板与 CSTOA 账号页
├── shell/          全局外壳、最近会话滑动选区、批量删除、顶部通知栈、Blue hour / AIR WebGL 背景
└── data/           共享类型（types.ts）、SharedWorker 聚合流客户端、乐观发送箱、24ms 流式播放器
```

依赖方向固定为 `routes -> 功能域 -> data`，浏览器代码不引入任何服务端运行时模块。静态托管路由对 `/assets/*` 哈希产物启用长缓存，对 HTML 页面导航请求读取 `cst-theme` Cookie（缺省为 `light`）注入 `<html data-theme>` 并返回 `index.html`，挂载后再由前端读取 `/api/settings` 同步。

## 流式渲染与 SSE 状态同步

1. **`SharedWorker` 32 会话分片聚合流（`event-stream.ts`、`event-stream-worker.ts`）**：
   - 同源多标签页通过 `SharedWorker` 共享后台 SSE 连接，规避浏览器 HTTP/1.1 同源 6 条并发连接上限。
   - Worker 启动先请求只读 `GET /api/state` 协商能力：`stage === "app_active"` 时直接进入通道待机锁定态。支持聚合流时按每 32 个活跃会话分配一个 `MultiplexShard` 连接 `/api/events/stream?clientId=<uuid>`，收到 `stream_ready` 后以单在途请求状态机调用 `PUT .../subscriptions` 同步订阅集合。
   - Worker 解析每帧完整向量游标用于断线 256 帧增量回放，收到定向 `reset` 时仅通知对应标签页重取快照。遇到已删会话 `404 session_not_found` 时剔除失效 ID 并继续同步其余会话。不支持 `SharedWorker` 的环境自动退回页内分片复用。
2. **平滑步进与流式连续性（`stream-player.ts`、`markdown.tsx`、`session-stream.ts`）**：
   - 流式增量按 24ms 步进消费缓冲（随积压加速并吸附词边界），流式与定稿共用同一套容器与逐词节点，代码高亮仅在定稿时执行。
   - `message_end` 原位承接完整消息，按消息时间戳保持轮次、过程与工具卡片组件身份不变，防止 HTTP 快照返回后组件重新挂载闪烁。

## 会话导出与离线文档管线

导出由前端预览工作区（`app/routes/export.tsx`、`/_export`）与后端 `crates/cst-server/src/exports/` 协同完成：

| 模块 | 职责 |
|---|---|
| `exports/snapshot.rs` | 单一时刻采样固定 `capturedAt` 与归档日期，投影当前分支可见条目与未闭合 BitLocker 强杀复查结果，执行 `browser_json` 脱敏与 `32 MiB` 体积检查 |
| `exports/store.rs`、`open.rs` | 维护 8 任务有界串行队列与 `session-exports/<id>.json` 原子登记。重启时清理 `work/` 并将中断任务标为失败。排他发布至 `artifacts/<日期>/sessions/`（同名分图整组追加 `-2`、`-3`），校验 SHA-256 与目录边界后拉起 `explorer.exe` |
| `exports/browser.rs` | 基于 `Chromiumoxide` 探测本机 Edge / Chrome（核验主版本 ≥111），单实例按需复用并为每任务创建隔离 `BrowserContext`，空闲 30 秒自动关闭清理 |
| `exports/render.rs`、`pagination.rs`、`html.rs` | CDP 拦截非同源静态请求，注入快照等待字体与图片就绪。PNG 按 `1240×3200` CSS 像素（2 倍密度）规划切点逐段截图。离线 HTML 提取已加载字体样式并以 4 路并发（配 `24 MiB` 跨任务缓存）将 `/fonts/*` 与 `/assets/*` 内嵌为 `data:` URL，封装严格 CSP |
| `app/exports/` | 预览切换保留已呈现 iframe，新快照在隐藏 iframe 内加载完字体与图片后以 250ms 淡入覆盖并沿用滚动位置。聊天与导出共用 `AssistantContent` 与 `TranscriptDisclosure`（离线 HTML 使用原生 `<details>`） |

## 会话预载、乐观发送与客户端存储

1. **两批预载与外壳边界**：`WorkspaceLayout` 锁定窗口高度并保持侧栏实例常驻。`SessionsProvider` 首次取得列表后分两批（每批最多 5 路）预载最新 10 个会话存入内存 LRU 缓存（上限 20 项），并通过 `PrefetchPageLinks` 预热聊天路由代码。
2. **乐观发送对账（`message-outbox.ts`、`use-message-outbox.tsx`）**：发送前先写入当前标签页 `sessionStorage`，断线或刷新后仅按原消息 ID 查询回执对账，不自动重发。后端确认接收或进入可见队列后移除本地备份。
3. **浏览器存储与草稿图片回收**：
   - `sessionStorage` 保存草稿正文与消息 ID（`cst-draft:<sessionId|new>`、`:id`、`:images`，以及编辑待发或失败项时暂存原草稿的 `:saved`）。
   - `IndexedDB` 按 SHA-256 保存未发送图片原始字节，配合 `localStorage` 引用表 `cst-draft-images:<ownerId>`、`Web Locks API` 标签页存活锁与 60 秒刷新宽限期回收孤儿字节。
   - `localStorage` 同步保存侧栏折叠态 `cst-ui-sidebar`、分支树过滤档位 `cst-tree-view` 与首次缺密钥 24 小时提示时间戳 `cst-model-key-prompt-at`。
4. **第三方 OAuth 弹窗隔离（`app/auth/`）**：打开第三方 OAuth 授权链接时先在点击同步上下文创建空白窗口并立即断开 `opener`，本机回环回调与手动粘贴授权码并行等待，先完成者生效。

## 分支树几何与过程分段

1. **分支树几何算法（`app/tree/branch-tree.ts`）**：对齐 `pi-session::tree` 规则，先在原始树上按活跃路径优先完成深度优先定序，再将五档过滤隐藏节点的可见后代重挂至最近可见祖先，计算每行缩进、分叉方框列与非末位兄弟延续竖线。
2. **过程分段计数（`app/cards/tool-kind.ts`、`turn-segments.ts`）**：将 `sys overview`、`web_search`、三个命令清单工具及 `bitlocker unlock/decrypt` 划分为独立常驻段，其余工具归入可折叠过程段并按实际完成数更新进度计数。

## 工具卡片转换

### 通用骨架与 Block 联合类型

`app/cards/map-tool.ts` 优先读取未裁剪的 `details`（缺失时回退 `content`）转换为统一的 `CardView` 结构：

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

整次 `isError: true` 统一渲染为错误原文块。正常或部分降级返回按下表映射：

| 工具 | 渲染形态／主模板 | 核心字段来源与转换规则 |
|---|---|---|
| `disk` | 概览组（`space/info/health/all`）<br>排行组（`usage`） | `space[]`、`volumes[]`、`physicalDisks[]`、`smart[]` 生成字段组。`usage` 将 `topDirs`、`topFiles`、`extAgg`、`staleFiles` 生成排行组，保留 `infoNotice`、`smartErrors`、`degradedFrom` |
| `ls` | 排行组 | `path/totalChildren/totalSize/method` 生成概况字段组，`entries[]` 生成大小排行组。任一子项不完整时 `pct` 保持为空 |
| `sys` | 特殊组件（`overview`）<br>排行组（`proc/gpu/io`）<br>概览组（`sensor`） | `overview` 走常驻组件（内存 MiB 换算 GiB）。`proc` 生成双排行，`gpu` 生成进程排行与适配器／NVIDIA／LHM 字段组，`io` 生成磁盘与进程排行，`sensor` 生成传感器、热区与频率字段组 |
| `startup` | 概览组 | 生成自启概况、`regItems[]`（映射禁用三态）、`startupFolders[]` 与 `services[]` 字段组 |
| `eventlog` | 概览组（列表类）<br>原文块（`detail`） | 列表类将 `events[]` 与 `counts[]` 分别转为字段组和来源排行组，附权限降级提示。`detail` 生成事件头字段组加 `msg` 原文块 |
| `driver` | 概览组 | `problem/find` 生成设备与汇总字段组。`core` 按网卡、蓝牙、音频、显示、服务、驱动分组。`external` 按外设与可移动盘分组 |
| `command_plan` / `runbook_deliver` / `runbook` | 特殊组件 | 优先取 `details.runbook`，展示风险档、版本号与命令列表。仅交付成功项展示可点击文件名链接 |
| `bitlocker` | 概览组（`status`）<br>特殊组件（`unlock/decrypt`） | `status` 按卷生成状态字段组。`unlock/decrypt` 合并卷实时状态、单卷 `operation`、顶层异常中断 `unknown` 复查原因及会话挂起交互请求 |
| `web_search` | 特殊组件 | 单行展示检索状态并比对 `successfulQueries` 与 `queryCount`，展开展示查询词列表 |
| `source_check` / `fetch_content` / `get_search_content` | 概览组 / 原文块 | 展示主张核验与来源排行，或抓取概况字段组加正文原文块／图片组与续读标识 |
| `fffind` / `ffgrep` / `read` | 原文块 / 图片例外 | `fffind` 与 `ffgrep` 展示命中正文文本块与截断提示。`read` 图片形态优先展示可预览图片，再附绝对路径 |

### 特殊工具交互

- **BitLocker 本地输入与确认**：`cst-server` 的 `BitLockerSessionBridge` 在会话内存中维护 3 分钟有效期的挂起请求，随快照与 SSE 下发。前端通过 `POST /api/sessions/:id/bitlocker/:requestId` 单次提交恢复密码或确认解密，绕过持久化幂等日志，格式错误返回 `400` 可原地修正重提。
- **Runbook 文件打开**：点击已交付文件名调用 `POST /api/sessions/:id/runbook/open`（仅传 `{ entryId, toolCallId }`），后端核对分支结果路径位于 `artifacts/` 内后直接调用系统 `notepad.exe` 打开。
