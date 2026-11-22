# Web 页面地址与接口

本文定义 Web 端的浏览器路由地址空间、前后端业务 HTTP/SSE 接口及幂等安全规则。行为要求见 [SPEC.md](SPEC.md)，软件端专属控制接口、聚合 SSE 向量游标帧格式与统一错误码总表见 [../server/API.md](../server/API.md)。

## 页面地址空间

| 路径 | 页面 | 说明 |
|---|---|---|
| `/` | 主页 | 欢迎语、新会话输入框与侧栏会话列表 |
| `/sessions` | 全部会话 | 按本地自然日四组展示的完整列表、重命名与批量删除 |
| `/s/<sessionId>` | 聊天工作台 | 会话消息流、过程折叠、工具卡片、输入队列与编辑器 |
| `/s/<sessionId>/tree` | 分支树 | 会话条目树、五档过滤（含标记过滤）、搜索与分支总结跳转 |
| `/s/<sessionId>/export` | 导出会话 | 格式、文档主题、过程包含项、只读预览与文件保存 |
| `/_export` | 只读文档 | 页面预览与后端 `Chromiumoxide` 渲染共用，无模型或账号请求 |
| `/dashboard` | 仪表盘 | 后续迭代：本地请求用量概览、趋势与日志 |
| `/login` | 登录页 | 居中面板（`cstoa` 扫码 / OAuth / APIKEY） |
| `/settings` | 模型服务设置 | 模型服务页（支持 `#accounts` 回跳锚点），管理已配置与失效服务 |
| `/settings/general` | 常规设置 | Web 主题切换（跟随系统／浅色／深色） |
| `/settings/models` | 模型偏好 | 新会话默认模型、启用模型范围勾选、筛选与拖拽／键盘排序 |
| `/settings/shortcuts` | 快捷键 | 聊天工作台快捷键一览 |
| `/settings/add` | 添加服务 | 更新模型目录、搜索未配置服务并连接、进入自定义服务 |
| `/settings/provider` | 服务配置 | 按 `provider` 与 `method` 执行授权、服务地址设置与移除凭据 |
| `/settings/custom` | 自定义服务 | 新建或编辑 `models.json` 中的兼容模型服务与模型元数据 |
| `/account` | 专属账号 | CSTOA 学号、姓名、可用额度点展示与登录／退出 |

## 会话接口

| 方法 | 路径 | 用途 |
|---|---|---|
| `GET` | `/api/sessions` | 会话列表（返回 `{ id, title, updatedAt, running }[]`） |
| `POST` | `/api/sessions` | 新建会话，可选指定初始模型与思考强度 |
| `GET` | `/api/sessions/creation/<requestKey>` | 只读核对创建回执，返回 `status: "missing" \| "started" \| "done"`，完成时附 `sessionId` |
| `GET` | `/api/sessions/<id>` | 会话完整快照（返回 `{ id, eventCursor, bitlocker, messages, entries, running, contextUsage, queue, ui }`，工作时长内嵌于 `assistant` 消息） |
| `PATCH` / `DELETE` | `/api/sessions/<id>` | 重命名会话 / 删除空闲会话及其未落盘快照、收件箱与图片附件目录 |
| `POST` | `/api/sessions/<id>/open`、`/close` | 打开会话（由其他进程持有写权时返回 `409`）/ 关闭通知（校验 ID 后返回 `{ closed: true }`，无需 `Idempotency-Key`，槽位由空闲 LRU 自动回收） |
| `POST` | `/api/sessions/<id>/messages` | 提交消息文本、技能标记与 Base64 图片（`delivery: "queue" \| "steer"`），持久化落盘后返回 `202 { item }` |
| `GET` | `/api/sessions/<id>/messages/<messageId>` | 只读核对收件箱回执，返回 `{ item }`（未找到为 `{ item: null }`），不触发投递 |
| `POST` | `/api/sessions/<id>/abort` | 停止当前轮次模型与工具执行，并暂停输入队列 |
| `PATCH` / `DELETE` | `/api/sessions/<id>/queue/<itemId>` | 编辑或删除待投递／失败的排队项（校验队列 `version`） |
| `PUT` | `/api/sessions/<id>/queue/order` | 按条目 ID 数组重排待投递队列（校验 `version`） |
| `POST` | `/api/sessions/<id>/queue/<itemId>/steer` | 将待投递项改为插话模式（校验 `version`） |
| `POST` | `/api/sessions/<id>/queue/<itemId>/retry` | 重试失败条目并恢复队列（校验 `version`） |
| `GET` | `/api/sessions/<id>/attachments/<attachmentId>` | 读取该会话已保存的图片字节 |
| `POST` | `/api/sessions/<id>/fork` | 从指定条目 `entryId` 派生新会话（目标为用户消息时取其父节点之前分支） |
| `GET` | `/api/sessions/<id>/tree` | 读取完整原始会话树节点与当前叶节点 |
| `POST` | `/api/sessions/<id>/tree/navigate`、`/tree/abort` | 执行分支导航（支持 `summarize` 与 `customInstructions`）/ 取消正在生成的分支总结（`/tree/abort` 无需 `Idempotency-Key`） |
| `GET` / `POST` | `/api/sessions/<id>/bitlocker`、`/bitlocker/<requestId>` | 读取当前 BitLocker 待处理请求 / 单次提交恢复密码、解密确认或取消（绕过持久化幂等表） |
| `POST` | `/api/sessions/<id>/runbook/open` | 传入 `{ entryId, toolCallId }`，校验已交付 TXT 位于 `artifacts/` 后调用系统记事本打开 |
| `POST` | `/api/sessions/<id>/ui/<requestId>/response` | 回答普通扩展发起的选择、确认或非敏感输入请求 |
| `POST` | `/api/sessions/<id>/compact` | 执行手动上下文压缩 |
| `GET` | `/api/sessions/<id>/events` | 单会话 SSE 事件流（支持 `Last-Event-ID` 续传） |

会话快照、导出预览与客户端 SSE 仅返回 `user`、`assistant`、`toolResult`、`branchSummary` 四类可见角色，自动过滤内部 `system` 元数据条目。底层 JSONL 与 `/tree` 接口保留完整原始节点链。宿主重启后遗留的 `delivering` 收件箱条目附 `uncertain: true`，禁止自动重放。

## 会话导出接口

导出文件保存至工具包根目录 `artifacts/<YYYY-MM-DD>/sessions/`：

| 方法 | 路径 | 返回与用途 |
|---|---|---|
| `POST` | `/api/sessions/<id>/exports/preview` | `200`。返回筛选与脱敏后的当前分支快照，不落盘 |
| `POST` | `/api/sessions/<id>/exports` | `202`。固定快照并入队创建后台导出任务，须携带 `Idempotency-Key` |
| `GET` | `/api/sessions/<id>/exports` | 最近 50 条导出记录，按快照时间倒序 |
| `GET` | `/api/exports/<exportId>` | 查询任务状态（`queued / rendering / completed / failed`）、选项、目录、文件列表或错误 |
| `GET` | `/api/exports/<exportId>/files/<index>` | 按下标读取已登记文件并核验 SHA-256。文件被修改返回 `409 export_file_changed`，缺失返回 `404 export_file_missing` |
| `POST` | `/api/exports/<exportId>/open` | 正文 `{}`（须带 `Idempotency-Key`）。由已完成记录推导并校验真实目录后启动本机资源管理器，返回 `{ launched: true }` |

请求参数与资源上限：

- 创建与预览请求体为 `{ format: "png" | "html", theme: "light" | "dark", includeThinking: boolean, includeTools: boolean }`，未知字段返回 `400`。
- 队列上限 8 个任务（超出返回 `429 export_busy`），单任务超时 120 秒（`504 export_timeout`），快照上限 `32 MiB`，产物合计上限 `200 MiB`（`413 export_too_large`）、PNG 最多 64 张。
- 后端通过 `Chromiumoxide` 驱动本机 Edge 或 Chrome（开发调试构建支持 `CST_EXPORT_BROWSER` 显式路径，正式发行构建仅探测系统注册表与标准安装目录，主版本下限 `111`）。PNG 视口宽 `1240` CSS 像素、2 倍缩放、单图高不超过 `3200` CSS 像素（文档总高上限 `100,000` CSS 像素）。离线 HTML 仅允许内嵌同源 `/fonts/*`、`/assets/*` 与 `data:` 资源（单资源上限 `16 MiB`，总数上限 128）。

| 任务失败码 | 含义 |
|---|---|
| `export_browser_missing` | 未找到可用 Edge / Chrome 或显式指定路径无效 |
| `export_browser_launch_failed` | 浏览器进程启动失败 |
| `export_browser_incompatible` | 检测到的 Chromium 主版本号低于 `111` |
| `export_document_failed`、`export_asset_unavailable` | 页面渲染失败，或字体／静态资源读取失败 |
| `export_asset_forbidden` | 样式引用了同源静态目录之外的资源 |
| `export_timeout`、`export_interrupted` | 渲染超时，或服务退出／重启中断了未完成任务 |

## 模型认证与账号接口

CSTOA 远端网关协议见 [../auth/API.md](../auth/API.md)。本机模型与认证接口如下（所有接口均隐藏明文密钥与令牌）：

| 方法 | 路径 | 用途 |
|---|---|---|
| `GET` | `/api/models` | 返回可用模型 `models`、完整目录 `catalog`、`defaultModel`、启用范围及可选 `sessionId` 的当前模型与支持思考档位 `thinkingLevels`。显式范围的 `scopeRules` 按保存顺序返回原表达式 `pattern` 和匹配项 `matches: { id, pattern }[]`；默认范围返回 `null` |
| `POST` | `/api/models/refresh` | 正文 `{}`，强制刷新已配置服务商目录，返回 `{ aborted, failedProviders }` |
| `POST` | `/api/models/select` | 切换指定 `sessionId` 的模型（未传 `sessionId` 时仅返回所选描述，不修改默认值） |
| `POST` | `/api/models/default` | 正文 `{ provider, modelId }`，保存三端共用的新会话默认模型 |
| `POST` | `/api/models/scoped` | 正文 `{ patterns: string[], empty?: boolean }`，保存启用模型范围并同步更新内存中已加载会话的范围视图 |
| `POST` | `/api/models/thinking` | 按 `sessionId` 修改思考强度档位 |
| `GET` | `/api/auth` | 返回全部服务商的配置状态、凭据类型、登录能力、`custom` 与 `requiresLogin` 标记 |
| `GET` / `POST` / `DELETE` | `/api/auth/custom/<provider>`、`/api/auth/custom` | 读取（仅含 `apiKeySet` 与 `revision`）、保存或删除自定义兼容模型服务 |
| `POST` / `GET` | `/api/auth/<provider>/oauth/start`、`/status`、`/respond`、`/cancel` | 启动或复用异步 OAuth 流程、轮询状态（含二维码链接）、提交步骤回答或取消流程 |
| `POST` / `GET` / `PUT` | `/api/auth/<provider>/api-key/start`、`/status`、`/respond`、`/cancel`、`/api-key` | 多步或直接保存服务 APIKEY（可选附 `baseUrl`） |
| `POST` | `/api/auth/<provider>/logout` | 清除指定服务商的本地凭据 |
| `GET` | `/api/account` | 查询 CSTOA 登录状态与成员学号、姓名（支持 `?refresh=1` 强制刷新） |
| `GET` | `/api/quota?provider=<id>` | 按需查询 `cstoa`、`deepseek`、`opencode-go` 额度。CSTOA 支持 `refresh=1` 强制刷新 |

## 设置与运行时接口

| 方法 | 路径 | 用途 |
|---|---|---|
| `GET` / `PATCH` | `/api/settings` | 读取或更新 `web-settings.json` 中的 Web 外观主题 |
| `GET` / `POST` | `/api/ui-hints`、`/api/ui-hints/missing-key-prompt/claim` | 读取 `.state/ui-hints.json` 运行态提示 / 原子申领三端共用的 24 小时缺密钥自动提示窗口（返回 `{ claimed, missingKeyPromptedAtMs }`） |
| `GET` | `/api/state` | 启动与通道协商快照（版本、`stage`、`channelMode`、`multiplexStream: true`、会话列表及命令技能清单） |
| `GET` | `/api/health` | 返回服务存活与退出标记（`connected`、`stopping`；软件端 Bearer 调用额外返回 `pid`、`processCreated`、`kitRoot`、`version`、`elevated`） |
| `GET` | `/api/lifecycle` | 返回 `{ stopping: boolean, version: string, activeExports: number, sessions: { id, title, running, pendingInboxCount }[] }` |
| `GET` / `PUT` | `/api/events/stream?clientId=<uuid>`、`/api/events/stream/<clientId>/subscriptions` | 多会话聚合 SSE 事件流与最多 32 会话动态订阅更新（协议见 [../server/API.md](../server/API.md#sse-帧协议与多会话聚合流)） |
| `GET` | `/api/events` | 单路全局状态 SSE 事件流 |
| `GET` | `/api/files?sessionId=<id>&query=<text>` | 按会话 `cwd` 扫描文件（跳过隐藏项与构建目录，限深 6 层、最多 4000 条、缓存 30 秒），返回最多 50 条匹配项供 `@` 补全使用 |
| `POST` | `/api/lifecycle/telemetry` | 传入 `{ reason: "pagehide" \| "pageshow" \| "online" }` 触发遥测提交与补发（2 秒合并，返回 `202 { accepted: true }`，无需 `Idempotency-Key`） |
| `POST` | `/api/lifecycle/exit` | 传入 `{ confirm: "stop" }`（无需 `Idempotency-Key`），停止全部会话、完成遥测定稿并退出后台服务 |

## 安全、幂等与错误处理约定

1. **回环、同源与通道校验**：服务仅监听 `127.0.0.1` 并校验 `Host`。网页端凭单次启动票据兑换绑定 `web_cookie_epoch` 的 `HttpOnly; SameSite=Strict` Cookie（`--standalone-web` 独立调试模式下直接签发当前世代 Cookie，无需启动票据）。写请求必须同时携带同源 `Origin` 与 `X-CST-Web-Request: 1`。后台处于 `AppActive` 时访问业务接口返回 `403 web_mode_inactive`。
2. **幂等与乐观锁**：除上表明确标注免除的接口外，一般写操作须携带 `Idempotency-Key`（1–128 位字母、数字、下划线或连字符），不同参数复用同一键或崩溃遗留未决记录均返回 `409 conflict`。收件箱修改校验整数 `version`，自定义服务修改校验 `revision`。BitLocker 提交通道使用一次性内存 `requestId`，永不落盘。
3. **体积上限与凭据预检**：原始图片单次合计上限 `12 MiB`（消息 JSON 请求体上限 `17 MiB`），普通 JSON 请求体上限 `128 KiB`。新建会话、发送消息与切换模型前均校验凭据有效性，缺失时返回 `409 credentials_required` 且不落盘收件箱。
4. **统一错误格式**：所有错误返回 `{ "error": { "code": "...", "message": "...", "sessionId"?: "..." } }`，完整状态码映射见 [../server/API.md](../server/API.md#统一错误响应与错误码)。
