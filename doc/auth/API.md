# 认证与模型服务接口

本文定义 CSTOA 远端服务接口、`AuthRuntime` 宿主编程接口，以及 OAuth 令牌载荷与本机额度映射结构。本机完整 `/api/models/*` 与 `/api/auth/*` HTTP 路由表见 [../web/API.md](../web/API.md#模型认证与账号接口)。

## CSTOA 服务端接口

服务端仓库为 `cstoa-api-fastapi`，基准域名 `https://www.cstoa.top`：

| 方法 | 路径 | 鉴权方式 | 用途 |
|---|---|---|---|
| `POST` | `/api/oauth/device_authorization` | 无（按 IP 限流） | 申请设备码，传入设备标识、平台、版本与 `scope`。返回 `device_code`、`user_code`、`verification_uri`、`verification_uri_complete`、`expires_in=300`、`interval=5` |
| `GET` | `/#/oauth/device`、`/api/oauth/device/info` | OA 网页会话 | 队员手机端扫码授权前端页与待审批设备信息查询接口 |
| `POST` | `/api/oauth/device/decision` | OA 登录 + TOTP | 批准或拒绝设备授权 |
| `POST` | `/api/oauth/token` | `device_code` 或 `refresh_token` | 换取或轮换 `access_token`。轮询错误遵循 RFC 8628（`authorization_pending`、`slow_down`、`expired_token`、`access_denied`） |
| `POST` | `/api/oauth/revoke` | OA 会话或设备令牌 | 吊销设备授权 |
| `POST` | `/api/oauth/introspect` | 接收端 `OA_SERVICE_TOKEN` | 遥测接收端校验 `access_token` 并解析 `mid` 与 `device_id` |
| `GET` / `DELETE` | `/api/member/devices`、`/api/member/devices/{device_id}` | OA 网页会话 | 查询或移除本人已授权设备 |
| `GET` | `/api/agent/profile` | Bearer `access_token` | 查询 OAuth 登录成员的学号、姓名与可用额度点 |
| `GET` | `/api/agent/key-profile` | Bearer `CSTOA APIKEY` | 校验 APIKEY 绑定的成员身份与可用额度点（已计入密钥限额） |
| `ANY` | `/api/agent/llm/v1/*` | Bearer `access_token` | OAuth 模型目录与推理代理，响应头透传 `x-oneapi-request-id` |
| `GET` / `POST` | `/v1/models`、`/v1/chat/completions` | Bearer `CSTOA APIKEY` | APIKEY 模型目录与推理接口 |
| `POST` | `/api/agent/llm/billing` | Bearer `access_token` 或 `APIKEY` | 按 `requestIds`（每批 ≤50）只读查询本人请求的实际消费额度点 |

## Rust 宿主接口（`AuthRuntime`）

`crates/cst-pilot` 导出 `AuthRuntime`，已由 `crates/cst-server/src/routes/{auth,models}.rs` 挂载至本机 HTTP 路由。`runtime()`、`login()` 与内部 Provider 实例仅供可信宿主进程调用，对外响应统一使用下表中的掩码结构：

| 方法 | 参数与返回契约 |
|---|---|
| `custom_model_defaults()` | 返回新建自定义模型的默认 `contextWindow`（`262,144`）与 `maxTokens` |
| `custom_providers()` / `custom_provider(id)` | 列出自定义服务 ID 或读取配置。已保存密钥仅返回布尔值 `apiKeySet` 并附当前 `revision` |
| `save_custom_provider(input)` | 接收 `id / name / baseUrl / api / apiKey? / models / revision?`。编辑已有服务必须携带匹配的 `revision`，保存后刷新 `ModelRuntime` 并返回新 `revision` |
| `delete_custom_provider(id, revision)` | 核对 `revision` 后删除自定义服务并刷新模型目录，保留其余服务与未知字段 |
| `with_provider_endpoint(provider_id, base_url, login)` | 临时设置或更新内置服务 `baseUrl` 并执行登录，登录失败时自动回滚原地址 |
| `logout(provider)` | 清除指定服务凭据。目标为 `cstoa` 时同步清空动态模型目录与账号额度缓存 |
| `account(cancel)` / `quota(provider, cancel)` | 查询账号身份或额度并支持取消。`cstoa` 资料与额度共享同一次上游请求与 60 秒缓存 |
| `account_snapshot(cancel)` | 一次性返回 `{ account, quota }` |
| `invalidate_account()` / `refresh(force)` | 主动清空账号额度缓存，或触发 CSTOA 目录检查（`force = true` 跳过有效期与失败退避并同步失效额度缓存） |

配置版本不匹配返回 `409 config_conflict`。文件损坏返回 `503 configuration_unavailable`，错误消息不含配置正文或远端原始响应。

## OAuth 令牌载荷与作用域

访问令牌使用 HMAC 签名，载荷字段如下：

| 字段 | 含义 |
|---|---|
| `mid` | 队员编号／学号 |
| `typ` | 固定为 `"agent"`，与 OA 网页会话凭据隔离 |
| `scope` | 已批准的作用域字符串数组（客户端当前固定申请 `["llm:chat"]`） |
| `device_id` | 绑定的工具包设备标识 |
| `pv` | 成员密码版本，改密后旧令牌立即失效 |
| `exp` / `jti` | 过期时间戳 / 令牌唯一标识 |

服务端已开放的 `scope` 值如下（`task:write` 为服务端预留未启用项）：

| 作用域（`scope`） | 权限范围 |
|---|---|
| `llm:chat` | 经 OA 代理调用模型服务，消耗本人额度 |
| `telemetry:write` | 以成员身份提交使用情况遥测 |
| `task:read` | 读取本人维修任务 |
| `repair:write` | 提交修机日志 |

## 本机资料与额度映射

本机 `/api/account` 与 `/api/quota` 将上游响应归一化为统一前端字段：

| 上游来源与字段 | 本机返回字段 | 映射规则 |
|---|---|---|
| CSTOA `data.memberId` | `profile.studentId` | 非空字符串时有效 |
| CSTOA `data.name` | `profile.name` | 姓名为空时保留 `studentId` 并标注姓名暂未提供 |
| CSTOA `data.balance` | `quota.balance`（`unit: "credit"`） | 仅在 `balanceAvailable=true` 且为非负整数时返回数值。`0` 表示有效零余额，查询失败返回 `balance: null` 与 `reason` |
| `provider=deepseek` | `quota.{balance, currency}` | 优先选取 `CNY` 可用余额，无 `CNY` 时取首个 `CNY/USD` 币种 |
| `provider=opencode-go` | `quota.windows.{rolling, weekly, monthly}` | 返回 5 小时、每周与每月三个用量窗口的 `status`、`percent` 与 `resetsAt` |
