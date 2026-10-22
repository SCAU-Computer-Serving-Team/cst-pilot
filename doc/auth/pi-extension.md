# OA 登录扩展

扩展通过 Pi 原生 Provider 接入 `cstoa`，提供 OAuth 与 APIKEY 两种登录方式。两端共用 Pi 的凭据、模型目录和推理实现，不修改 Pi 内核。业务规则见[OA 登录与模型接入](README.md)。

## 职责与文件

| 文件 | 职责 |
|---|---|
| `agent/home/extensions/oauth/index.ts` | 注册原生 `cstoa` Provider |
| `provider.ts` | 双方式登录、凭据对应的固定模型地址、复用 Pi Chat Completions 适配器 |
| `oa.ts` | OAuth 设备码申请、轮询、令牌交换和刷新 |
| `device.ts` | U 盘设备标识 |
| `models.ts` | 动态模型目录、元数据、凭据隔离、缓存与发布 |
| `sync.ts` | TUI/Web 自动刷新，合并同一运行实例的并发请求 |
| `test/` | OAuth、模型目录和真实 Pi 加载测试 |

凭据由 Pi 保存到 `agent/home/auth.json`，动态目录由 Pi 保存到 `models-store.json`。目录只包含模型元数据和凭据摘要，不包含令牌或密钥。Web 本人资料与额度由 `web/server/api/account.ts` 查询，浏览器不读取已有凭据。

## OAuth

1. `login()` 申请设备码，用 `device_code` 事件交给 TUI 或 Web 展示。
2. `verificationUri` 使用服务端返回的完整授权链接，手机打开后预填数字码。
3. 手机完成 OA 登录、TOTP 校验与批准。扩展按服务端间隔轮询，并响应取消、拒绝、过期和减速。
4. 成功返回访问令牌、可选刷新令牌和毫秒到期时间。预取模型后由 Pi 保存凭据与目录。
5. Pi 在访问令牌进入刷新窗口后调用扩展刷新。刷新请求使用 Pi 提供的取消信号，凭据轮换和跨进程互斥由 Pi 管理。

默认不下发刷新令牌；记住模式的七天从首次授权起算。其他有效期和吊销规则见[登录有效期](README.md#登录有效期)。

## APIKEY

1. 使用 Pi 原生秘密输入步骤获取队员提供的 CSTOA APIKEY。
2. 调用固定的 `/api/agent/key-profile` 校验成员归属和凭据状态。积分为零不阻止登录。
3. 预取 `/v1/models`，由 Pi 保存 `type=api_key` 凭据。
4. 模型请求直接使用 OA 域名下的 `/v1`。OAuth 请求使用 `/api/agent/llm/v1`，两种凭据不混用请求地址。

CSTOA 禁用自定义服务地址。其他服务的 APIKEY、自定义地址和 OAuth 行为保持独立。

## 动态模型

Provider 的 `refreshModels(context)` 通过 `context.publish()` 发布清单与缓存。成功清单允许为空，空清单移除旧模型；格式、网络和 HTTP 错误保留同一凭据的有效缓存。

- 登录后预取，进入模型界面或发送输入时自动检查。成功缓存五分钟，失败后等待三十秒再尝试。
- 换凭据或服务地址时清除不匹配的目录，缓存不能使其他账号的模型可用。
- 模型 ID 来自网关；能力与上下文参数复用 Pi 的已知目录，未知模型使用保守参数。上游返回的 URL 和请求头不作为连接配置。
- 推理使用 Pi 内置 Chat Completions 实现，保留工具调用、流式输出和取消行为。模型本地费用为零，真实积分由 New API 扣除。

`PI_OFFLINE=1` 继续关闭 Pi 全局启动联网。项目只主动更新 CSTOA，不开启其他 Provider 的启动联网。

## 验证与发行

```powershell
npm run check --prefix agent
npm run test:oauth --prefix agent
node --test agent/home/extensions/web/test/cstoa-credentials.test.ts
node --test agent/home/extensions/web/test/cstoa-login.e2e.ts agent/home/extensions/web/test/account.e2e.ts
```

浏览器测试先构建 Web。隔离测试覆盖双方式登录、实际 Pi 推理路径、积分刷新、目录持久化与凭据隔离，入口见[账号验证](../test/account-report.md)。真实手机授权和实际积分扣减需使用获准的测试账号单独验证。

发行包复制扩展源码，排除个人凭据和设备标识。首次使用后在工具包目录生成运行态文件。
