# OA 登录扩展

状态：实现规格，尚未开发。更新：2026-09-20。适配项目锁定的 pi 0.85.1，业务约定见 [OA 登录与模型接入](README.md)。

扩展通过 `pi.registerProvider("cstoa", { oauth })` 接入原生 `/login`，不修改 pi 内核。

## 职责

| 事项 | 负责方 |
|---|---|
| 登录入口、网址与数字码显示 | pi，扩展调用 `callbacks.onDeviceCode` |
| 申请设备码、轮询、令牌交换 | 扩展 |
| 凭据保存、刷新触发、并发刷新互斥 | pi，凭据位于 `agent/home/auth.json` |
| 刷新请求 | 扩展实现 `refreshToken(credentials, signal)` |
| 模型请求 | pi 的 OpenAI 兼容适配器，令牌由 `oauth.getApiKey` 提供 |
| 设备标识 | 扩展维护 `agent/home/device.json` |

## 文件

```text
agent/home/extensions/oauth/
|-- index.ts       注册 provider
|-- oa.ts          设备授权与令牌交换
|-- device.ts      设备标识
`-- package.json
```

只使用 Node 内置模块与全局 `fetch`。

## 登录与刷新

1. `login(callbacks)` 请求设备码，用 `onDeviceCode({ userCode, verificationUri, intervalSeconds, expiresInSeconds })` 显示授权信息。
2. 扩展按 OA 给定的 interval 轮询，处理待确认、减速、拒绝、过期与用户取消。pi 内部轮询工具没有公开导出。
3. 成功后返回 `{ access, refresh, expires }`，`expires` 是毫秒时间戳，由 pi 保存凭据。
4. 默认模式没有刷新令牌时返回 `refresh: ""`；进入刷新窗口后提示重新授权。
5. pi 在取凭据时发现剩余有效期不足五分钟，调用 `refreshToken`。扩展发出刷新请求并返回轮换后的凭据，网络请求使用传入的 `signal`；该信号包含十五秒超时。
6. 网络或超时错误提示检查网络；凭据失效时要求重新 `/login`。

登录轮询响应 `callbacks.signal`，刷新请求响应 `signal`。扩展不另建后台刷新循环。

## 模型与凭据取用

| provider 字段 | 值 |
|---|---|
| `baseUrl` | `https://cstoa.top/api/agent/llm/v1` |
| `api` | `openai-completions` |
| `models` | OA 网关提供的可用模型清单 |
| `oauth.getApiKey` | `(credentials) => credentials.access` |

其他 OA 业务需要当前访问令牌时，使用 `ctx.modelRegistry.getProviderAuth("cstoa")`，从结果的 `auth.apiKey` 取值。该调用按需触发 pi 的刷新流程；调用方处理失败，不保存令牌副本。

## 错误提示

| 场景 | 行为 |
|---|---|
| 授权拒绝或设备码过期 | 提示重新开始授权 |
| 网络失败或超时 | 提示检查网络 |
| 401，凭据失效 | 清理本地凭据并提示重新授权 |
| 403，权限不足 | 提示权限不足 |
| 设备吊销或改密 | 提示重新授权或联系管理员 |
| 额度不足 | 提示额度不足，不自动重试 |
| 凭据写入失败 | 提示存储问题，不声称已成功保存登录态 |

## 测试用例

| 用例 | 说明 |
|---|---|
| 首次授权 | 全新 U 盘：`/login` → 手机输入数字码并确认 → 模型可用 |
| 无任务调用 | 未选任务、未创建修机会话时，权限与额度满足即可调用模型 |
| 跳电脑 | 同一 U 盘换电脑、盘符变化：登录态按有效期延续，不要求重新授权 |
| 记住到期 | 记住模式 7 天（自首次授权）到期后要求重新授权 |
| 并发刷新 | 多路并发请求只触发一次刷新（pi 文件锁） |
| 本地退出 | `/logout` 只清本地凭据；服务端吊销后独立副本也失效 |
| 拔盘、写盘失败 | 明确提示，不误报为令牌盗用 |
| 拒绝 | 授权页拒绝后收到 `access_denied` |
| 超时 | 5 分钟未确认，提示重新开始 |
| 刷新 | 记住模式下 pi 自动刷新 |
| 吊销 | 个人中心吊销后请求被拒并提示重新登录 |
| 改密 | 改密后旧令牌立即失效 |
| 额度不足 | 代理端点返回额度错误，界面提示 |
| 断网 | 明确的网络错误提示 |

## 便携运行与发行

1. 通过 `pi.cmd` 启动，`PI_CODING_AGENT_DIR` 指向工具包的 `agent/home`。设备标识与凭据均保存在此目录。
2. `PI_OFFLINE=1` 只关闭 pi 的启动联网，不阻止授权、刷新和模型请求。
3. `pack/pack.mjs` 复制扩展目录。发行包不含个人凭据与设备标识，首次运行后生成。
4. 验收范围见 [OA 登录与模型接入](README.md#本期验收)。
