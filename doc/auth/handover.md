# OAuth 扩展交接

状态：待开发，规格已定稿。更新：2026-10-03。接手人从本文开始，按阅读顺序过完即可动手。

## 交付物

| 文件 | 职责 | 规格章节 |
|---|---|---|
| `agent/home/extensions/oauth/index.ts` | 注册 provider `cstoa`，接进 pi 原生 `/login` | [pi-extension.md](pi-extension.md)「职责」 |
| `agent/home/extensions/oauth/oa.ts` | 设备授权申请、轮询、令牌交换与刷新 | [README.md](README.md)「范围与流程」「OA 接口」 |
| `agent/home/extensions/oauth/device.ts` | 设备标识的生成与持久化 | [README.md](README.md)「令牌与设备标识」 |
| `agent/home/extensions/oauth/package.json` | 已就位 | — |

骨架文件已建好，注释里标了每步的协议要求。只使用 Node 内置模块与全局 `fetch`。

Web UI 的 OAuth 登录面板是第二阶段交付，取设备码信息的方式待定（见文末）。

## 阅读顺序

1. [README.md](README.md)：业务协议——设备流、令牌形状、有效期、作用域、安全边界。
2. [pi-extension.md](pi-extension.md)：扩展实现规格——pi 侧与扩展侧的职责分界、文件结构、错误提示、测试用例。
3. 参考：`agent/home/extensions/telemetry/credential.ts`——遥测上传复用同一访问令牌，取令牌方式已实现。

## 已定决策

以下各项在规格里定稿，开发时直接执行，不重新讨论。

| 决策 | 出处 |
|---|---|
| 授权协议用 RFC 8628 设备授权流，手机扫码授权 | README「范围与流程」 |
| 实现技术与 pi 内置 OAuth 机制一致：`registerProvider` 的 `oauth` 字段（login / refreshToken / getApiKey，接口定义在 pi 的 `core/extensions/types.d.ts`），设备码展示与轮询 UI 由 pi 内核的 provider-composer 原生提供 | pi-extension「职责」 |
| 授权页 2FA 用 Authenticator（TOTP），强制校验 | README「安全边界」 |
| 访问令牌 2 小时；「记住 7 天」可选，刷新令牌每次轮换 | README「登录有效期」 |
| 令牌 HMAC 签名，载荷含 mid / typ / scope / device_id / pv / exp / jti | README「令牌与设备标识」 |
| 模型调用走 OA 代理，网关密钥不下发 | README「模型接入路径」（D11） |
| 验收标准 | README「本期验收」+ pi-extension「测试用例」 |

## 环境

1. 本地运行见仓库根 [AGENTS.md](../../AGENTS.md)「运行」：`pi.cmd`，或 `PI_CODING_AGENT_DIR` 指向本仓库 `agent/home` 调试。
2. OA 端点未就绪时，本机起 mock 设备授权服务联调（HTTP + JSON 即可，参照遥测 e2e 的 mock 模型服务的做法）。

## 外部依赖

OA 服务端在独立仓库 [cstoa-api-fastapi](https://github.com/SCAU-Computer-Serving-Team/cstoa-api-fastapi)，任务拆分见其 `docs/oa-tasks.md`。有一处缺口要 OA 侧补：

- `POST /api/oauth/introspect`：遥测接收端（cst-pilot-server 仓库）靠它解析上报者身份，尚未列入 README「OA 接口」表。对接时向 OA 侧提出，响应需区分 `active`、`mid`、`device_id` 与 `expired` / `invalid` / `revoked` / `password-changed`。

## 待定项

- Web UI 取设备码信息的方式：pi 的会话事件或扩展 API，二选一，实现登录面板前定。
