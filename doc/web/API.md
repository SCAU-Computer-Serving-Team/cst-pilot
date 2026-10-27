# Web 页面地址与接口

本文定义 Web 端的浏览器路由地址空间、HTTP/SSE 接口契约及安全与幂等规则。行为要求见 [SPEC.md](SPEC.md)。

## 页面地址空间

| 路径 | 页面 | 说明 |
|---|---|---|
| `/` | 主页 | 欢迎语、新会话输入框与侧栏会话列表 |
| `/s/<sessionId>` | 聊天工作台 | 会话消息流、过程折叠、工具卡片、输入队列与编辑器 |
| `/s/<sessionId>/tree` | 分支树 | 会话条目树、五档过滤、搜索、折叠与分支总结导航 |
| `/dashboard` | 仪表盘 | 后续迭代：本地请求用量概览、趋势与日志 |
| `/login` | 登录页 | 居中面板（`cstoa` 扫码 / Pi OAuth / APIKEY） |
| `/settings` | 模型服务设置 | 默认设置页（兼容 `/settings#accounts`），管理已配置与失效服务 |
| `/settings/general` | 常规设置 | Web 主题切换（浅色／深色／跟随系统） |
| `/settings/models` | 模型偏好 | 启用模型范围勾选、搜索、按服务筛选与拖拽／键盘排序 |
| `/settings/shortcuts` | 快捷键 | 聊天工作台快捷键一览 |
| `/settings/add` | 添加服务 | 搜索未配置服务并连接，顶部提供自定义服务入口 |
| `/settings/provider` | 服务配置 | 按 `provider` 与 `method` 执行 OAuth / APIKEY 配置、自定义地址与移除凭据 |
| `/settings/custom` | 自定义服务 | 新建或编辑 `models.json` 中的兼容模型服务与模型元数据 |
| `/account` | 专属账号 | CSTOA 学号、姓名展示与登录／退出 |

## 会话接口

| 方法 | 路径 | 用途 |
|---|---|---|
| `GET` | `/api/sessions` | 会话列表（ID、标题、创建时间、更新时间、占用方、运行状态） |
| `POST` | `/api/sessions` | 新建会话，可选指定初始模型与思考强度 |
| `GET` | `/api/sessions/<id>` | 会话完整快照（消息、工作时长、运行状态、队列版本与条目、待答 UI 请求、BitLocker 待处理请求） |
| `PATCH` / `DELETE` | `/api/sessions/<id>` | 重命名会话 / 删除空闲会话及其图片附件目录 |
| `POST` | `/api/sessions/<id>/open`、`/close` | 打开会话（由 TUI 持有时返回 409）/ 取消当前页面订阅 |
| `POST` | `/api/sessions/<id>/messages` | 提交消息文本、技能标记与 Base64 图片（`delivery: "queue" \| "steer"`） |
| `POST` | `/api/sessions/<id>/abort` | 停止当前轮次模型与工具执行，并暂停输入队列 |
| `PATCH` / `DELETE` | `/api/sessions/<id>/queue/<itemId>` | 编辑或删除待投递／失败的排队项（校验队列 `version`） |
| `PUT` | `/api/sessions/<id>/queue/order` | 按条目 ID 数组重排待投递队列（校验 `version`） |
| `POST` | `/api/sessions/<id>/queue/<itemId>/steer` | 将待投递项改为插话模式（校验 `version`） |
| `POST` | `/api/sessions/<id>/queue/<itemId>/retry` | 重试失败条目并恢复队列（校验 `version`） |
| `GET` | `/api/sessions/<id>/attachments/<attachmentId>` | 读取该会话已接受保存的图片字节 |
| `POST` | `/api/sessions/<id>/fork` | 从指定用户消息 `entryId` 之前派生新会话 |
| `GET` | `/api/sessions/<id>/tree` | 读取会话分支树与当前叶节点 |
| `POST` | `/api/sessions/<id>/tree/navigate`、`/tree/abort` | 执行分支导航（支持 `summarize` 与 `customInstructions`）/ 取消正在生成的分支总结 |
| `GET` / `POST` | `/api/sessions/<id>/bitlocker`、`/bitlocker/<requestId>` | 读取当前 BitLocker 待处理请求 / 一次性提交本地恢复密码、解密确认或取消 |
| `POST` | `/api/sessions/<id>/runbook/open` | 按助手条目与调用 ID 匹配 `runbook` / `runbook_deliver` 的结果，在系统记事本中打开已交付的 TXT 清单 |
| `POST` | `/api/sessions/<id>/ui/<requestId>/response` | 回答扩展发起的选择、确认或输入请求 |
| `POST` | `/api/sessions/<id>/compact` | 执行手动上下文压缩 |
| `GET` | `/api/sessions/<id>/events` | 单会话 SSE 增量事件流（支持 `Last-Event-ID` 续传） |

### 清单打开请求

`POST /api/sessions/<id>/runbook/open` 正文为 `{ entryId, toolCallId }`，不接受浏览器指定文件路径。服务端按该次交付结果取路径；未导出的方案不能打开，文件不存在、无权限或记事本启动失败分别返回对应错误。

### 输入与队列请求

- 消息请求正文为 `{ "id": "...", "text": "...", "delivery": "queue", "images": [], "skill": "..." }`；`images` 与 `skill` 可省略。接受后返回 HTTP `202` 与 `{ "item": ... }`，同一会话内按消息 `id` 和内容去重。
- 队列操作正文包含快照中的整数 `version`；编辑还需 `text`，重排还需完整待排队条目的 `ids` 数组。成功返回 `{ "queue": ... }`，版本过期或条目状态不允许操作时返回 `409`。
- `failed` 条目可编辑、删除或显式重试。重启时保留未确认的 `delivering` 条目，不自动重放；其后续处理见[待决议题](../TASKS.md#工程与安全边界)。

## 模型认证与账号接口

CSTOA 远端接口与字段映射见 [../auth/API.md](../auth/API.md)。本机 Web 接口如下（所有接口均不返回已保存的明文密钥或令牌）：

| 方法 | 路径 | 用途 |
|---|---|---|
| `GET` | `/api/models` | 可用模型清单、启用范围（`enabled=null` 表全启用）、未匹配的 `unavailableEnabled` 及可选 `sessionId` 当前选择 |
| `POST` | `/api/models/select` | 保存跨端默认模型；传入 `sessionId` 时同步切换该会话模型 |
| `POST` | `/api/models/scoped` | 更新启用模型范围与顺序（全选可用模型时归一为 `null`） |
| `POST` | `/api/models/thinking` | 按 `sessionId` 设置思考强度 |
| `GET` | `/api/auth` | 所有 Provider 的配置状态、凭据类型、登录能力、`custom` 与 `requiresLogin` 标记 |
| `GET` / `POST` / `DELETE` | `/api/auth/custom/<provider>`、`/api/auth/custom` | 读取（仅返回 `apiKeySet` 与 `revision`）、新建／编辑、删除 `models.json` 中的自定义服务 |
| `POST` / `GET` | `/api/auth/<provider>/oauth/start`、`/status`、`/respond`、`/cancel` | 启动或复用 OAuth 流程、读取状态（含设备码与二维码链接）、提交步骤回答、取消流程 |
| `POST` / `GET` / `PUT` | `/api/auth/<provider>/api-key/start`、`/status`、`/respond`、`/cancel`、`/api-key` | 多步或直接提交 APIKEY（可选附 `baseUrl`） |
| `POST` | `/api/auth/<provider>/logout` | 清除指定 Provider 的本地凭据 |
| `GET` | `/api/account` | 读取 CSTOA 登录状态与成员资料（`studentId`、`name`） |
| `GET` | `/api/quota?provider=<id>` | 按需查询 `cstoa`、`deepseek`、`opencode-go` 额度；CSTOA 支持 `refresh=1` 强制刷新 |

## 设置与运行时接口

| 方法 | 路径 | 用途 |
|---|---|---|
| `GET` / `PATCH` | `/api/settings` | 读取或更新 Web 设置（如主题 `theme`） |
| `GET` | `/api/state` | 启动快照（工具包版本、会话列表、可用命令与技能清单） |
| `GET` | `/api/health` | 返回连接状态及是否正在退出（`connected`、`stopping`） |
| `GET` | `/api/lifecycle` | 返回退出状态与当前会话运行情况 |
| `GET` | `/api/events` | 全局 SSE 事件流（会话列表、写权占用、设置、凭据与模型目录变化） |
| `GET` | `/api/files?sessionId=<id>&query=<text>` | 按会话 `cwd` 枚举文件，忽略隐藏项及常见依赖／构建目录；限深 6 层、最多扫描 4000 条、缓存 30 秒。按 `query` 过滤后返回最多 50 条与扫描总数，供 `@` 补全使用 |
| `POST` | `/api/lifecycle/telemetry` | 页面 `pagehide` / `pageshow` / `online` 触发遥测检查与心跳（2 秒内合并） |
| `POST` | `/api/lifecycle/exit` | 停止所有会话、完成保存与遥测定稿后关闭 Web 服务与待机宿主 |

## 安全、幂等与错误处理约定

1. **回环与同源防护**：服务仅绑定 `127.0.0.1` 并校验 `Host`；写请求须携带同源 `Origin` 与 `X-CST-Web-Request: 1`，带请求体的接口使用 `Content-Type: application/json`。跨站请求与 CORS 预检被拒绝。
2. **幂等与版本并发控制**：一般写操作携带 `Idempotency-Key`，长度为 1–128 位，仅允许字母、数字、下划线和连字符。同一键用于不同操作、或重启后操作结果未确认时返回 `409`；消息提交按会话内消息 `id`、正文、图片、技能与投递方式去重。队列使用整数 `version`，自定义服务编辑使用 `revision`，过期返回 `409`。BitLocker 本地凭据提交使用一次性 `requestId`，不写入持久化幂等日志。
3. **体积限制**：消息提交的原始图片合计不超过 `12 MiB`（仅限 PNG/JPEG/GIF/WebP），消息 JSON 请求体不超过 `17 MiB`；其他调用 JSON 正文的接口默认上限为 `128 KiB`。
4. **统一错误结构**：所有接口错误返回 `{ "error": { "code": "...", "message": "..." } }`，不含敏感配置原文或凭据。
