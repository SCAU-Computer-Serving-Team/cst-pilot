# 页面地址与接口

本文定义 Web 的地址空间与服务端接口。会话如何运行见[会话运行与并行](session-runtime.md)，前端工程见[前端工程](frontend.md)，视觉取值见 [DESIGN.md](../../../DESIGN.md)。

## 地址空间

| 地址 | 页面 | 说明 |
|---|---|---|
| `/` | 主页 | 入口，含会话列表 |
| `/s/<sessionId>` | 聊天工作台 | 会话 ID 在地址里 |
| `/s/<sessionId>/tree` | 分支树 | 对应 TUI 的 `/tree`，呈现与行布局规则见[分支树](branch-tree.md) |
| `/dashboard` | 仪表盘 | MVP 后实现，字段口径见[仪表盘](dashboard.md) |
| `/login` | 登录页 | 居中面板 |
| `/settings` | 设置 | 入口在账号菜单 |
| `/settings/provider` | 模型服务配置 | 接收 `provider`、`method`；使用设置行布局承载凭据表单 |
| `/account` | CSTOA 账号信息 | 学号、姓名、额度与专属账号登录、退出 |

导航行为：真实链接、可复制地址、可在新标签页打开、支持刷新与前进后退。地址只在本机服务运行时有效，不是公开分享链接。

## 接口

前缀 `/api`，同源。常规响应为 JSON，事件流用 SSE，会话导出返回下载内容。

### 会话

| 方法 | 路径 | 用途 |
|---|---|---|
| GET | `/api/sessions` | 会话列表：ID、标题、更新时间、占用方、运行状态 |
| POST | `/api/sessions` | 新建会话 |
| GET | `/api/sessions/<id>` | 会话详情：消息、执行状态、队列版本与条目、待回答的扩展提问 |
| PATCH | `/api/sessions/<id>` | 重命名 |
| DELETE | `/api/sessions/<id>` | 删除 |
| POST | `/api/sessions/<id>/open` | 进入会话；复用已有 Web 实例，若仍由 TUI 持有则返回 409 |
| POST | `/api/sessions/<id>/close` | 注销本页订阅；执行实例的占用按[释放规则](session-runtime.md#写权与会话串行)处理 |
| POST | `/api/sessions/<id>/messages` | 提交消息与图片；保存已接受图片后，返回确认顺序及附件引用 |
| POST | `/api/sessions/<id>/abort` | 停止该会话的当前执行 |
| PATCH | `/api/sessions/<id>/queue/<itemId>` | 编辑尚未投递的排队项，携带预期队列版本 |
| DELETE | `/api/sessions/<id>/queue/<itemId>` | 取消尚未投递的排队项，携带预期队列版本 |
| PUT | `/api/sessions/<id>/queue/order` | 按条目 ID 重排，携带预期队列版本 |
| POST | `/api/sessions/<id>/queue/<itemId>/steer` | 将尚未投递的条目改为插话，携带预期队列版本；不停止当前执行 |
| GET | `/api/sessions/<id>/attachments/<attachmentId>` | 读取该会话已接受的图片，供历史回放与其他页面展示 |
| POST | `/api/sessions/<id>/fork` | 从指定 `entryId` 派生会话，返回新会话 ID |
| GET | `/api/sessions/<id>/tree` | 读取分支树 |
| POST | `/api/sessions/<id>/tree/navigate` | 导航到指定 `entryId`，对应 `/tree` 的选择操作 |
| GET | `/api/sessions/<id>/export` | 导出当前会话，供浏览器下载 |
| POST | `/api/sessions/<id>/ui/<requestId>/response` | 回答扩展的选择、确认、输入或多行编辑请求 |
| POST | `/api/sessions/<id>/compact` | 压缩，对应 `/compact` |
| GET | `/api/sessions/<id>/events` | 该会话的事件流 |

### 模型与登录

行为见[登录与模型配置](auth.md)。接口不返回已有凭据。

| 方法 | 路径 | 用途 |
|---|---|---|
| GET | `/api/models` | 可用模型清单与启用范围：`enabled` 为 `null` 表示全启用，`unavailableEnabled` 列出未匹配的已保存启用模式；指定 `sessionId` 时附该会话当前选择 |
| POST | `/api/models/select` | 保存 TUI 与 Web 共用的默认模型；提供 `sessionId` 时同时切换该会话，主页省略 `sessionId` |
| POST | `/api/auth/<provider>/api-key/start` | 开始 Pi 原生 API KEY 交互，可选预填 key 与自定义 baseUrl |
| GET | `/api/auth/<provider>/api-key/status` | 读取当前密钥登录步骤与版本 |
| POST | `/api/auth/<provider>/api-key/respond`、`/cancel` | 回答当前步骤或取消，校验 flowId 与 promptId |
| POST | `/api/models/scoped` | 改启用范围，对应 `/scoped-models`；覆盖全部可用模型时归一为默认范围（`null`） |
| POST | `/api/models/thinking` | 按 `sessionId` 设置思考强度；无会话的主页选择在新建会话时提交 |
| GET | `/api/auth` | 各 Provider 的登录状态、凭据类型、`supportsApiKey`、`supportsOAuth` 与 `requiresLogin`；不返回凭据。模型服务拒绝凭据后标记需重新登录 |
| PUT | `/api/auth/<provider>/api-key` | 提交 `key`；可附 `baseUrl` 覆盖所选 Provider 的模型地址。配置与凭据由后端保存，响应不返回密钥 |
| POST | `/api/auth/<provider>/logout` | 清除该 Provider 的本地登录状态 |

| 方法 | 路径 | 用途 |
|---|---|---|
| POST | `/api/auth/<provider>/oauth/start` | 发起或复用该服务的进行中流程，返回 `flowId` 和首个交互步骤 |
| GET | `/api/auth/<provider>/oauth/status` | 读取流程状态、递增 `revision`、设备码剩余时间、授权链接和当前输入步骤 |
| POST | `/api/auth/<provider>/oauth/respond` | 提交 `{ flowId, promptId, value }`；过期步骤返回 409，无效选项返回 400 |
| POST | `/api/auth/<provider>/oauth/cancel` | 取消流程；可携带 `flowId`，旧页面不能取消新流程 |

### 专属账号

`GET /api/account` 只读取 cstoa OAuth 状态，返回 `signedIn`、`requiresLogin`、`profile` 与 `quota`。左下角「账号信息」进入 `/account`；账号退出只调用 cstoa 的 logout，其他模型服务保持登录。设置入口始终可用。

学号和姓名的 Agent 资料接口尚未接入，`profile.supported=false`，`studentId` 与 `name` 为 `null`；额度同样为不支持和空值。界面明确标注未提供或未接入。现有 OA `/api/member/info` 使用浏览器 Cookie 鉴权，不能直接复用 Agent OAuth 凭据。

### 设置

| 方法 | 路径 | 用途 |
|---|---|---|
| GET | `/api/settings` | 读设置 |
| PATCH | `/api/settings` | 改设置 |

cstoa 额度与仪表盘用量接口在后续阶段确定。

### 运行时

| 方法 | 路径 | 用途 |
|---|---|---|
| GET | `/api/state` | 启动快照：版本、会话列表与运行状态、连接状态、可用命令；当前查看对象由页面地址决定 |
| GET | `/api/events` | 全局事件流：会话列表变化、占用变化、设置变化 |
| GET | `/api/files?sessionId=<id>` | 项目文件清单，供输入框 `@` 引用补全 |
| GET | `/api/quota` | 按 `provider` 查询额度：DeepSeek 返回账户余额，OpenCode Go / Go Plus 返回 5 小时、每周、每月用量窗口 |

`/api/state` 提供启动快照；页面按需读取相关数据。

### 约定

1. 错误统一为 `{ error: { code, message } }`，HTTP 状态表达语义。会话仍由 TUI 持有或正在被其他实例写入时返回 409。
2. 会话 ID 使用稳定会话 ID。地址与响应都不出现记录文件的绝对路径。
3. 写操作带 `Idempotency-Key`；消息提交另以消息 ID 去重。同一消息 ID 重复提交，只有会话、正文、图片与提交方式一致时才算重试，否则返回冲突。执行中断后的重复请求不会重新执行。
4. 内置命令走对应接口。普通消息中的斜杠文本按原文交给模型。调用已开放的诊断技能时，消息体显式传 `skill`（范围见命令规格），后端才展开技能。逐条对照见[命令与功能对应](commands.md)。
5. Web 服务自身无访问账号；模型服务的登录状态由后端凭据决定。**仅监听本机回环地址**，不提供局域网或远程访问。
6. 服务端控制允许的工具集合。新建、恢复、切换会话和修改设置时，前端都不能扩大这个集合。
7. 凭据由后端保存与使用。浏览器只提交新凭据、读取状态；已有密钥不随会话快照返回，也不写进浏览器存储。失效时提示重新登录，不自动重试模型调用。
8. 只提供界面所需数据。不提供 home 目录或任意路径的下载接口，不返回完整配置对象。
9. 模型输出、工具文本与机主文件内容按不可信内容处理，不作为任意 HTML 或脚本执行。
10. 校验 `Host`、请求来源与允许的方法。写接口要求同源 `Origin` 和自定义请求头，不向外站放行 CORS 预检；导航与事件流分别处理，不声称仅监听回环即可完成请求鉴权。
11. 事件流带递增序号。重连时用 `Last-Event-ID` 从断点续传；序号超出保留范围时返回信号，前端改为全量重取。
12. 产物文件名带内容哈希，配长缓存；`index.html` 不缓存。本地回环不做压缩与 `Range`。
13. 队列编辑、删除、排序和转为插话由后端串行处理。版本过期或条目已投递时返回 409，并要求重新读取队列；转为插话只投递一次，不中断当前输出。去重键不替代版本检查。
14. 修改会话的接口串行处理；被 TUI 持有时返回 409。删除与压缩要求会话空闲；分支导航先停止当前生成，压缩期间拒绝导航。
15. 扩展提问带 `requestId`，通过会话快照与事件流发布。后端只接受一次有效回答，失效请求不再投递给扩展。
16. 消息提交的图片用 `{ mimeType, data }` 表示，`data` 为 Base64；只接受 PNG、JPEG、GIF、WebP，原始字节合计不超过 12 MiB，整条 JSON 请求不超过 17 MiB。后端保存原始字节。读取接口只接受该会话已引用的附件 ID；删除会话时清理附件目录。

## 双版本

**MVP 只有现代版。** `/web` 直接打开，没有档位参数；服务端只选一个产物目录，地址共用一套、不加前缀，端口只有一个。

兼容版（Tailwind 3.4、Chromium 86）在功能完善后增加，入口命令与切换方式届时确定，基线见[浏览器基线](frontend.md#浏览器基线)。
