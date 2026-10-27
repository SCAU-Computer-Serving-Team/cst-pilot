# CSTOA 接入

CST Pilot 的 `cstoa` Provider 支持手机 OAuth 授权与 CSTOA APIKEY。两种方式共用成员身份、积分和模型选择，凭据由 Pi 管理，TUI 与 Web 使用同一份配置。

## 实现入口

| 文件 | 功能 |
|---|---|
| `agent/home/extensions/oauth/index.ts`、`provider.ts` | 注册 Provider，处理双方式登录与模型请求地址 |
| `agent/home/extensions/oauth/oa.ts` | 设备码申请、轮询、令牌交换与刷新 |
| `agent/home/extensions/oauth/device.ts` | 设备标识生成与保存 |
| `agent/home/extensions/oauth/models.ts`、`sync.ts` | 动态模型、凭据隔离、缓存与自动刷新 |
| `agent/home/extensions/web/server/api/oauth-flow.ts` | 将 Pi 登录交互转换为 Web 授权步骤 |
| `agent/home/extensions/web/server/api/account.ts` | 查询本人身份与积分，供账号和额度接口共用 |

OAuth 设备协议使用 Node 内置模块与 `fetch`；Provider 和推理复用 Pi 公共接口。遥测通过 `agent/home/extensions/telemetry/credential.ts` 读取有效的 OAuth 令牌，APIKEY 不作为遥测身份凭据。

## 服务端接口

服务端位于 [cstoa-api-fastapi](https://github.com/SCAU-Computer-Serving-Team/cstoa-api-fastapi)。

| 接口 | 功能 |
|---|---|
| `/api/oauth/*` | OAuth 设备授权、令牌刷新与吊销 |
| `GET /api/agent/profile` | OAuth 本人身份与积分 |
| `GET /api/agent/key-profile` | APIKEY 绑定成员与可用积分 |
| `/api/agent/llm/v1/*` | OAuth 模型目录与推理代理 |
| `GET /v1/models`、`POST /v1/chat/completions` | APIKEY 模型目录与推理，消耗成员积分并受密钥额度上限约束 |
| `POST /api/oauth/introspect` | 遥测接收端验证 OAuth 上传身份；本机工具不持有内省服务密钥 |

## 规格与验证

- [账号与模型规格](README.md)：设备授权、APIKEY、有效期、积分和安全规则。
- [Pi 扩展实现](pi-extension.md)：Provider、凭据、动态目录和推理适配。
- [Web 登录规格](../web/SPEC/auth.md)：登录、取消、账号、启用模型和退出。
- [账号测试](../test/account.md)：隔离测试分工、核对项目和真实账号验证方法。
- [遥测测试](../test/telemetry.md)：OAuth 上传身份、请求账单和双端接收验证方法。

隔离测试使用 `agent/home/extensions/oauth/test/mock-oa.ts`，不读取用户凭据。真实手机授权和积分扣减使用获准的测试账号单独验证。
