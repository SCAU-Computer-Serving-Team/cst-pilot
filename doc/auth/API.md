# 认证与模型服务接口

本文定义 CSTOA 服务端接口、令牌载荷结构及本机 Web 认证／账号字段映射。Web 端完整 HTTP 路由表见 [../web/API.md](../web/API.md#模型认证与账号接口)。

## CSTOA 服务端接口

服务端仓库为 `cstoa-api-fastapi`，基准域名 `https://www.cstoa.top`：

| 方法 | 路径 | 鉴权方式 | 用途 |
|---|---|---|---|
| `POST` | `/api/oauth/device_authorization` | 无（按 IP 限流） | 申请设备码，传入设备名、平台、Agent 版本与 `scope`；返回 `device_code`、6 位 `user_code`、`verification_uri`、`verification_uri_complete`、`expires_in=300`、`interval=5` |
| `GET` | `/oauth/device` | OA 网页会话 | 队员手机端授权页 |
| `POST` | `/api/oauth/device/decision` | OA 登录 + TOTP | 批准或拒绝设备授权 |
| `POST` | `/api/oauth/token` | `device_code` 或 `refresh_token` | 换取或轮换 `access_token`；轮询错误遵循 RFC 8628（`authorization_pending`、`slow_down`、`expired_token`、`access_denied`） |
| `POST` | `/api/oauth/revoke` | OA 会话或设备令牌 | 吊销设备授权 |
| `POST` | `/api/oauth/introspect` | 接收端 `OA_SERVICE_TOKEN` | 遥测接收端校验 `access_token` 并解析 `mid` 与 `device_id` |
| `GET` / `DELETE` | `/api/agent/devices`、`/api/agent/devices/{id}` | OA 网页会话 | 查询或移除本人已授权设备 |
| `GET` | `/api/agent/profile` | Bearer `access_token` | 查询 OAuth 登录成员的学号、姓名与可用额度点 |
| `GET` | `/api/agent/key-profile` | Bearer `CSTOA APIKEY` | 校验 APIKEY 绑定的成员身份与可用额度点（已计入密钥限额） |
| `ANY` | `/api/agent/llm/v1/*` | Bearer `access_token` | OAuth OpenAI 兼容模型目录与推理代理，响应头透传 `x-oneapi-request-id` |
| `GET` / `POST` | `/v1/models`、`/v1/chat/completions` | Bearer `CSTOA APIKEY` | APIKEY 模型目录与推理接口 |
| `POST` | `/api/agent/llm/billing` | Bearer `access_token` 或 `APIKEY` | 按 `requestIds`（每批 ≤50）只读查询本人请求的实际消费额度点 |

## OAuth 令牌载荷与作用域

访问令牌采用 HMAC 签名，载荷字段如下：

| 字段 | 含义 |
|---|---|
| `mid` | 队员编号／学号 |
| `typ` | 固定为 `"agent"`，与 OA 网页会话凭据隔离 |
| `scope` | 已批准的作用域字符串 |
| `device_id` | 绑定的工具包设备标识 |
| `pv` | 成员密码版本（改密后旧令牌立即失效） |
| `exp` / `jti` | 过期时间戳 / 令牌唯一标识 |

| 作用域（`scope`） | 权限范围 |
|---|---|
| `llm:chat` | 经 OA 代理调用模型服务，消耗本人额度 |
| `task:read` / `task:write` | 读取本人维修任务 / 接单、到达、完成（写操作需确认） |
| `repair:write` | 提交修机日志 |

## 本机资料与额度映射

本机 Web 后端将 CSTOA `/api/agent/profile` 与 `/api/agent/key-profile` 的响应映射为前端结构：

| OA 响应字段 | 本机 `/api/account` 或 `/api/quota` 字段 | 映射规则 |
|---|---|---|
| `data.memberId` | `profile.studentId` | 非空字符串时有效 |
| `data.name` | `profile.name` | 姓名为空时保留 `studentId` 并标注姓名暂未提供 |
| `data.balance` | `quota.balance`（`unit: "credit"`） | 仅当 `balanceAvailable=true` 且为非负安全整数时返回数值；`0` 为有效零余额，查询失败时返回 `balance: null` 与 `reason` |

其他支持额度查询的服务：

- `provider=deepseek`：返回账户余额与币种。
- `provider=opencode-go`：返回 5 小时、每周、每月三个用量窗口百分比与重置时间。
