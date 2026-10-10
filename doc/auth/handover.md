# OAuth 扩展交接

扩展、Web 登录面板和账号查询已完成隔离验收。真实授权、模型调用与多队员资料仍待生产验收。更新：2026-10-10。

## 实现入口

| 文件 | 职责 |
|---|---|
| `agent/home/extensions/oauth/index.ts` | 注册 `cstoa`，接入 Pi 原生 `/login` |
| `agent/home/extensions/oauth/oa.ts` | 设备授权申请、轮询、令牌交换与刷新 |
| `agent/home/extensions/oauth/device.ts` | 设备标识的生成与保存 |
| `agent/home/extensions/web/server/api/oauth-flow.ts` | 调用 Pi 登录，向 Web 转发授权步骤与设备码 |
| `agent/home/extensions/web/server/api/account.ts` | 查询本人资料，供账号与额度接口共用 |

OAuth 扩展只使用 Node 内置模块与全局 `fetch`。凭据由 Pi 管理；遥测取用同一令牌的入口为 `agent/home/extensions/telemetry/credential.ts`。

## 阅读顺序

1. [认证规格](README.md)：设备流、有效期、作用域与安全边界。
2. [Pi 扩展方案](pi-extension.md)：Pi 与扩展的职责、登录和刷新接口。
3. [Web 登录规格](../web/SPEC/auth.md)：扫码、其他 OAuth 和 API KEY 的页面行为。
4. [账号接口](../web/SPEC/app-router.md#专属账号)：身份、额度、失效与退出。

## 本机联调

1. 使用仓库根 `pi.cmd` 启动，运行配置见 [AGENTS.md](../../AGENTS.md#运行)。
2. 隔离测试使用 `agent/home/extensions/oauth/test/mock-oa.ts`，避免读取用户凭据或访问生产模型。
3. 执行 `npm run test:oauth --prefix agent`；Web 全流程和局部账号验收见[账号验证](../test/account-report.md#可重复入口)。

## OA 对接

OA 服务端位于 [cstoa-api-fastapi](https://github.com/SCAU-Computer-Serving-Team/cstoa-api-fastapi)。接口职责如下。

| 接口 | 用途 | 对接依据 |
|---|---|---|
| `/api/oauth/*` | 设备授权、令牌刷新与吊销 | [认证规格](README.md#oa-接口) |
| `GET /api/agent/profile` | 本人身份与额度 | [OA #124](https://github.com/SCAU-Computer-Serving-Team/cstoa-api-fastapi/pull/124)、[账号验证](../test/account-report.md) |
| `/api/agent/llm/v1/*` | 模型代理 | [认证规格](README.md#oa-接口) |
| `POST /api/oauth/introspect` | 接收端验证上传身份 | [遥测验证](../test/telemetry-report.md#遥测运行检查)；本机工具不持有内省服务密钥 |

## 待验收

| 范围 | 验收条件 |
|---|---|
| 真实授权与模型调用 | 使用 OA 测试账号和手机 TOTP 完成设备授权，并核对模型请求；生产写操作前取得授权 |
| 身份与额度 | 多队员及实际扣减后的额度，见[账号验证](../test/account-report.md#未验证边界) |
| 遥测 | 真实队员双端入库、备份与恢复，见[遥测验证](../test/telemetry-report.md#正式启用前的验收) |
