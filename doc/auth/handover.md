# OAuth 扩展交接

状态：扩展与 Web 面板已实现（mock e2e + 浏览器全流程验证通过），待真机联调；规格已定稿。更新：2026-10-03。接手人从本文开始，按阅读顺序过完即可动手。

## 交付物

| 文件 | 职责 | 规格章节 |
|---|---|---|
| `agent/home/extensions/oauth/index.ts` | 注册 provider `cstoa`，接进 pi 原生 `/login` | [pi-extension.md](pi-extension.md)「职责」 |
| `agent/home/extensions/oauth/oa.ts` | 设备授权申请、轮询、令牌交换与刷新 | [README.md](README.md)「范围与流程」「OA 接口」 |
| `agent/home/extensions/oauth/device.ts` | 设备标识的生成与持久化 | [README.md](README.md)「令牌与设备标识」 |
| `agent/home/extensions/oauth/package.json` | 已就位 | — |

扩展文件已实现，只使用 Node 内置模块与全局 `fetch`；测试见 `agent/home/extensions/oauth/test/`（`npm run test:oauth`）。

Web UI 的 OAuth 登录面板已实现；取设备码信息的方式：Web server 直接调用内核 `modelRuntime.login()`，消费 `notify` 的 `device_code` 事件（详见 [pi-extension.md](pi-extension.md)「登录与刷新」）。

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

## 实现进度

1. 扩展已完成：设备标识（device.json）、RFC 8628 设备流、令牌轮换刷新、mock 单测与内核 e2e。
2. Web 面板已完成：登录页扫码 tab（进入即申请、二维码 + 倒计时 + 刷新/取消）、服务端 `oauth/start|status|cancel` 路由、`/api/events` 事件（`oauth_device_code` / `oauth_result`）。测试见 `agent/home/extensions/web/test/oauth-api.test.ts`。
3. 真机联调：需 OA 测试账号 + 手机 TOTP 完成一次真实设备授权；生产写操作前先报主审查。
4. 已知外部依赖：OA 模型代理当前要求 `X-CSTOA-Session`（修机会话），未接入会话前扫码登录只能到"凭据签发"，模型调用会被 400 拒绝；见 [README.md](README.md) 的模型接入说明。
