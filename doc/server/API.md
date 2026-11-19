# 后台服务与通道接口

本文定义 `cst-server` 的本机服务发现格式、双通道鉴权规则与软件端控制接口（`/api/app/*`），并列出聚合 SSE 帧协议与统一错误码。前后端共用的会话、模型、认证与导出 HTTP 路由表见 [../web/API.md](../web/API.md)。

## 服务发现与双通道鉴权

后台服务启动后在 `.state/server.json` 原子写入服务发现信息（该目录受 `PathGuard` 封锁，禁止模型工具读取）：

```json
{
  "pid": 18420,
  "port": 52831,
  "token": "<64位十六进制随机AppToken>",
  "startedAt": "2026-11-17T14:00:00Z"
}
```

请求校验规则：

1. **回环与同源校验**：所有请求必须满足 `Host == 127.0.0.1:<port>`（否则返回 `403 invalid_host`）。携带 `Origin` 头时必须等于 `http://127.0.0.1:<port>`（否则返回 `403 invalid_origin`），拒绝一切 CORS `OPTIONS` 预检。
2. **软件端鉴权**：携带 `Authorization: Bearer <token>`（常量时间比对，缺失或无效返回 `401 unauthorized`，可附带 `X-CST-Client: app` 显式声明软件端身份）。
3. **网页端鉴权**：浏览器首次访问 `/?ticket=<once>` 兑换绑定当前 `web_cookie_epoch` 的 `HttpOnly; SameSite=Strict` 会话 Cookie 并 `303` 重定向移除地址栏票据。所有非 `GET`/`HEAD` 写请求须同时携带同源 `Origin` 与 `X-CST-Web-Request: 1`。

## 软件端专属控制接口（`/api/app/*`）

仅允许通过 Bearer `token` 鉴权的软件端调用：

| 方法 | 路径 | 用途与返回 |
|---|---|---|
| `GET` | `/api/app/events` | 软件端存活监测与全局状态 SSE 流。建连递增活跃连接计数，保持当前 `ChannelMode` 不变。末条连接断开启动 6 秒退出宽限期 |
| `POST` | `/api/app/web/open` | 持有 `ActivityLease` 切换至 `WebActive`（已激活时幂等复用），生成 30 秒单次票据，返回 `{ url, mode: "web_active" }` |
| `POST` | `/api/app/web/close` | 持有 `ActivityLease` 切回 `AppActive`，递增 `web_cookie_epoch` 作废旧 Cookie，广播 `channel_mode_changed` 并断开全部 Web SSE 连接，返回 `{ mode: "app_active" }` |
| `GET` / `PATCH` | `/api/app/settings` | 读取或按顶层对象局部合并更新 `agent/home/desktop.json`（`PATCH` 持有 `ActivityLease` 并广播 `scope: "desktop"` 的 `settings_changed`），返回桌面外观、默认视图、侧栏折叠与窗口位置尺寸 |
| `GET` | `/api/app/update/status` | 要求 `X-CST-Client: app`，返回 `{ currentVersion, status: UpdateStatus, lastReport }` |
| `POST` | `/api/app/update/check` | 要求 `X-CST-Client: app`，检查更新并广播 `update_changed`，返回 `UpdateStatus`（未启用在线更新时原样返回 `200`） |
| `POST` | `/api/app/update/prepare` | 要求 `X-CST-Client: app`，分配 `operationId` 流式下载归档并双重验签解压至暂存目录，返回 `UpdateStatus` |
| `POST` | `/api/app/update/cancel` | 要求 `X-CST-Client: app`，取消下载或撤销已准备的升级包，清理暂存目录后回到 `available`。传入 `{ operationId }` 时仅取消对应操作；标识不匹配返回 `409 conflict` |
| `POST` | `/api/app/update/launch` | 要求 `X-CST-Client: app`，传入 `{ appPid, appCreated, ui? }`。核验活跃 `/api/app/events` 连接与宿主进程身份。关闭 `ActivityGate` 后检查运行中会话、未决收件箱与活动导出。全部空闲时拉起 `cst-updater.exe` 并在返回 `{ receipt }` 后异步收尾退出服务 |
| `POST` | `/api/app/exit` | 关闭桌面窗口时调用，跳过断线宽限期，暂停收件箱、停止活跃任务并在 5 秒预算内定稿遥测后退出服务，返回 `{ exited: true }` |

## SSE 帧协议与多会话聚合流

服务端严格区分顶层 SSE `event:` 名称与 `data:` JSON 内的业务 `type` 字段：

| 端点 | 顶层 `event:` | `id:` 游标格式 | `data:` JSON 载荷 |
|---|---|---|---|
| `GET /api/events`<br>`GET /api/app/events`<br>`GET /api/events/stream` | `state` | `<epoch>:<seq>`（单流）<br>完整向量游标（聚合流） | `sessions_changed`、`models_changed`、`auth_changed`、`settings_changed`（附 `scope: "desktop" \| "web" \| "shared"`）、`update_changed`（附 `status: UpdateStatus`）、`usage_changed`、`quota_changed`、`channel_mode_changed`、`oauth_state`、`oauth_device_code`、`oauth_result`、`stream_ready`（聚合流初始同步帧） |
| `GET /api/sessions/<id>/events`<br>`GET /api/events/stream` | `session` | `<epoch>:<seq>`（单流）<br>完整向量游标（聚合流） | 单会话流直接发送事件对象，聚合流外包 `{ sessionId, event }`，包含脱敏后的 `AgentSessionEvent`、`ui_requests` 与 `bitlocker_requests_changed` |
| `GET /api/sessions/<id>/events`<br>`GET /api/events/stream` | `queue` | `<epoch>:<seq>`（单流）<br>完整向量游标（聚合流） | 单会话流直接发送 `InboxSnapshot`，聚合流外包 `{ sessionId, queue }` |
| 全部 SSE 端点 | `heartbeat` / `reset` | 无 `id` | `heartbeat` 每 20 秒发送 `{}`。游标缺失或超出 256 条窗口时发送 `reset`（聚合流附 `{ scope: "global" \| "session", sessionId? }`）驱动客户端重拉快照 |

多会话聚合流接口契约：

- `GET /api/events/stream?clientId=<uuid>`：URL 不带会话列表参数。建连并完成向量游标 `global:<e0>:<s0>,<sid1>:<e1>:<s1>,...` 回放检查后，固定发送 `stream_ready` 携带当前服务端记录的 `subscribedSessions`。若服务曾重启导致返回空数组，客户端立即重发订阅恢复。
- `PUT /api/events/stream/<clientId>/subscriptions`：携带 `Idempotency-Key` 提交 `{ sessions: string[], lastEventIds?: Record<string, string> }`（单 `clientId` 上限 32 个会话），原子更新订阅集合并触发新增会话的增量回放或定向 `reset`。

## 统一错误响应与错误码

所有异常返回 `{ "error": { "code": "...", "message": "...", "sessionId"?: "..." } }`，不含敏感原文：

| HTTP 状态码 | `error.code` | 触发条件 |
|---|---|---|
| `400` | `invalid_json` / `invalid_request` / `invalid_path` / `invalid_hint` / `invalid_parent_process` / `invalid_bitlocker_input` / `invalid_export` / `empty_export` / `export_layout_invalid` / `export_asset_forbidden` / `runbook_invalid_file` | JSON 语法错误、未知字段、参数或路径不合法、升级宿主进程身份不匹配，以及 BitLocker 动作／48 位密码格式错误（保留原 `requestId` 可重试）或导出／清单校验失败 |
| `401` | `unauthorized` | 软件端请求缺失 `Authorization: Bearer <token>` 或令牌不匹配 |
| `403` | `invalid_host` / `invalid_origin` / `web_mode_inactive` / `runbook_access_denied` / `export_directory_invalid` / `export_directory_denied` | 非回环 Host、跨站 Origin、缺少同源头，或在 `AppActive` 下通过浏览器访问业务接口，以及清单／导出目录越界或无权限 |
| `404` | `not_found` / `session_not_found` / `runbook_missing` / `export_not_found` / `export_file_missing` / `export_directory_missing` | 会话、附件、导出任务、导出产物目录或已交付清单不存在 |
| `405` | `method_not_allowed` | 请求 HTTP 方法不被目标路由支持 |
| `409` | `web_mode_active` / `session_owned` / `credentials_required` / `conflict` / `config_conflict` / `already_compacted` / `nothing_to_compact` / `expired_request` / `export_file_changed` / `export_not_completed` / `stopping` / `update_in_progress` / `update_busy` / `update_not_available` / `update_not_ready` / `app_not_connected` / `session_busy_for_update` | 通道锁定或跨进程写权占用。模型缺凭据、幂等键或版本号冲突、手动压缩边界。BitLocker 请求过期或导出文件已被修改，以及服务正在退出、升级交接进行中或升级前置状态不满足 |
| `413` | `invalid_request` / `export_too_large` / `export_too_tall` | 请求体超出上限（普通 JSON `128 KiB`，消息请求 `17 MiB` / 原始图片 `12 MiB`）或导出产物超限 |
| `429` | `export_busy` | 会话导出队列已满（上限 8 个任务） |
| `500` / `507` | `server_error` / `storage_unavailable` / `storage_full` / `runbook_unavailable` / `export_failed` / `export_document_failed` / `export_record_unavailable` | 介质只读写保护、磁盘空间已满，或内部导出与清单读取失败 |
| `503` | `configuration_unavailable` / `static_missing` / `runbook_launch_failed` / `export_browser_missing` / `export_browser_incompatible` / `export_browser_launch_failed` / `export_launch_failed` / `export_stopping` / `export_interrupted` | 配置文件语法损坏、前端静态产物缺失、外部程序启动失败，或导出任务因退出中断 |
| `504` | `export_timeout` | 会话导出单任务渲染超过 120 秒预算 |
