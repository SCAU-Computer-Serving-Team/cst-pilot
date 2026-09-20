# pi 扩展技术方案

状态：草案。更新：2026-09-19。

对应 [OA 授权与令牌](README.md) 的 M1。实现范围：cst-pilot 发行版内的 pi 扩展。

## 结论

1. 用 pi 原生的自定义 provider OAuth 机制实现：扩展通过 `pi.registerProvider("cstoa", { oauth })` 注册。
2. 登录界面、凭据存储、刷新触发都由 pi 负责；扩展实现 `login` 与 `refreshToken` 两个回调，不维护独立刷新流程。
3. 模型流量走 OA 代理（方案 S）：provider 的 `baseUrl` 指向 OA 的 OpenAI 兼容端点，`getApiKey` 返回 OA 访问令牌。
4. 登录界面分两种：TUI 用 pi 原生设备码界面（网址 + 6 位数字码）；Web UI 显示二维码（编码 verification_uri_complete，扫描直达授权页）。上游 issue #9774 只涉及 TUI。
5. 扩展只额外维护设备标识；任务与日志请求用 `ctx.modelRegistry.getProviderAuth("cstoa")` 取当前令牌，不另存副本。

## 与 pi 原生能力的分工

| 事项 | 负责方 |
|---|---|
| 登录界面（网址、6 位数字码） | pi，`onDeviceCode` 回调（不自动打开浏览器） |
| Web UI 登录面板 | 扩展提供设备码信息，Web 前端渲染二维码与倒计时 |
| 凭据存储与刷新触发 | pi，`auth.json` 与 `refreshToken` 回调；剩余有效期不足 5 分钟才触发 |
| 刷新请求实现 | 扩展：调用 OA 的 refresh grant，并响应取消信号（15 秒超时） |
| 设备码轮询 | 扩展：pi 的轮询工具未从公共 `/oauth` 入口导出 |
| 任务选择、日志等 OA API 调用 | 扩展，令牌取自 `ctx.modelRegistry.getProviderAuth` |
| 二维码 | Web UI：扩展提供数据、前端渲染；TUI：暂不做（上游 issue #9774） |

## 扩展结构

```
agent/home/extensions/oauth/
|-- index.ts       入口：registerProvider 与命令注册
|-- oa.ts          OA 设备流与令牌交换（login/refresh）
|-- device.ts      设备标识（agent/home/device.json）
|-- tasks.ts       任务列表与选择
`-- package.json
```

## 登录与刷新

1. `/login` 选择 "CSTOA OA"，pi 调用扩展的 `login(callbacks)`。
2. `login`：POST `/api/oauth/device_authorization`，然后用 `callbacks.onDeviceCode({ userCode, verificationUri, intervalSeconds, expiresInSeconds })` 交给 pi 展示。
3. 按 interval 轮询 `/api/oauth/token`，成功后返回 `{ access, refresh, expires }`。
4. pi 把凭据写入 `auth.json`；取凭据时若剩余有效期不足 5 分钟才调用 `refreshToken(credentials, signal)`，不是后台定时刷新。
5. 刷新走 OA 的 refresh grant，轮换令牌；`signal` 带 15 秒超时，网络请求必须把它传给 fetch。失败时提示重新 `/login`。

约定：

1. 默认模式下 OA 不下发 refresh；扩展以空字符串满足 pi 的字符串类型要求，进入刷新窗口时提示重新授权。
2. 开启「记住 7 天」时返回 refresh，由 pi 持久化与轮换；7 天从首次授权起算。
3. 扩展不缓存令牌副本；任务与日志请求每次经 `getProviderAuth` 取当前令牌。
4. TUI 与 Web UI 共用同一个 login()；Web UI 从扩展取 `{userCode, verificationUriComplete, expiresInSeconds}` 自行渲染二维码。

## 模型调用（方案 S）

| provider 字段 | 值 |
|---|---|
| baseUrl | https://cstoa.top/api/agent/llm/v1 |
| api | OpenAI 兼容 |
| models | 网关可用模型清单，与 OA 约定 |
| oauth.getApiKey | `(credentials) => credentials.access` |

1. OA 代理端点校验 `llm:chat` 作用域与额度，转发到 New API 并记录审计。
2. 流式响应按 OpenAI 兼容格式透传。

## OA API 调用（任务与日志）

1. 任务与日志请求用 `ctx.modelRegistry.getProviderAuth("cstoa")` 取当前访问令牌，需要时会触发 pi 的刷新流程。
2. 不写任何令牌缓存文件；凭据只有 pi 的 `auth.json` 一份。
3. 错误分类处理：401 令牌失效、403 权限或任务状态不允许、额度不足、网络失败分别提示。
4. 任务绑定（M1）：`/cst-task` 列出本人任务，选择后创建修机会话；每场都要重新选择。

## 错误处理

| 场景 | 行为 |
|---|---|
| 授权被拒 | 提示已拒绝，重新执行 /login |
| 授权超时 | 提示重新开始（设备码 5 分钟有效期） |
| 网络失败 | 提示检查网络，无离线模式 |
| 401 令牌失效 | 清理本地凭据并提示重新授权 |
| 403 权限或任务状态不允许 | 提示对应原因，不当作登录失效 |
| 设备被吊销、改密 | 请求被拒后提示重新授权或联系管理员 |
| 额度不足 | 提示额度不足，不自动重试 |
| 写盘失败、突然拔盘 | 提示凭据文件未写完并重新登录；不当作令牌盗用 |

## 打包与合规

1. pack.mjs 整目录复制 extensions/，无需白名单改动。
2. M1 不引入第三方库（二维码暂不做）。
3. pi.cmd 注释更新：说明 OAuth 与离线开关的关系。

## 测试用例

| 用例 | 说明 |
|---|---|
| 首次授权 | 全新 U 盘：/login → 手机输入数字码并确认 → 选任务 → 模型可用 |
| 跨电脑 | 同一 U 盘换电脑、盘符变化：登录态按有效期延续，任务必须重新选 |
| 记住到期 | 记住模式 7 天（自首次授权）到期后要求重新授权 |
| 并发刷新 | 多路并发请求只触发一次刷新（pi 文件锁） |
| 本地退出 | /logout 只清本地凭据；服务端吊销后独立副本也失效 |
| 拔盘、写盘失败 | 明确提示，不误报为令牌盗用 |
| Web UI 二维码 | Web UI 弹出二维码，手机扫码直达授权页并预填数字码 |
| 拒绝 | 授权页拒绝后收到 access_denied |
| 超时 | 5 分钟未确认，提示重新开始 |
| 刷新 | 记住模式下 pi 自动刷新 |
| 吊销 | 个人中心吊销后请求被拒并提示重新登录 |
| 改密 | 改密后旧令牌立即失效 |
| 额度不足 | 代理端点返回额度错误，界面提示 |
| 断网 | 明确的网络错误提示 |

## 待核实

| 编号 | 事项 |
|---|---|
| V1 | 已解决：`ctx.modelRegistry.getProviderAuth("cstoa")` 可读取当前令牌，无需缓存文件 |
| V2 | 命令注册与刷新提示的最佳挂载点 |
| V3 | Web UI 接收设备码信息的桥接方式（会话事件或扩展 API） |
