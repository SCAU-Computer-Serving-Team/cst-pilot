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

OA 服务端在独立仓库 [cstoa-api-fastapi](https://github.com/SCAU-Computer-Serving-Team/cstoa-api-fastapi)，任务拆分见其 `docs/oa-tasks.md`。对接边界：

- `POST /api/oauth/introspect` 已在 OA 远端源码提供，部署与遥测接收端联调仍需确认。该接口用于服务间鉴权，本机工具不持有内省服务密钥。
- 本人资料使用 `GET /api/agent/me`，返回 `id` 与 `name`。本项目调用与 OA 本地接口代码已完成，OA 生产发布待确认。

## 实现进度

1. 扩展已完成：设备标识（device.json）、RFC 8628 设备流、令牌轮换刷新、mock 单测与内核 e2e。
2. Web 面板已完成：cstoa 优先扫码；Pi 其他 OAuth 的授权链接、设备码、回贴码和选择步骤接入同一登录流程。接口与账号边界见 [页面地址与接口](../web/SPEC/app-router.md#模型与登录)。测试见 `agent/home/extensions/web/test/oauth-api.test.ts` 与 `oauth-flows.test.ts`。
3. 真机联调：需 OA 测试账号 + 手机 TOTP 完成一次真实设备授权；生产写操作前先报主审查。
4. 账号资料：本项目本机后端已接入 OA `GET /api/agent/me`，账号页显示学号与姓名，并处理未上线、凭据失效与网络失败。OA 接口代码与测试已在本地完成，生产尚未发布；额度保留占位。验证见[接入与遥测验证](../test/telemetry-report.md)。
5. OA 远端源码已提供无修机会话的模型代理路径。生产部署与真实登录后的模型调用仍需联调确认。
