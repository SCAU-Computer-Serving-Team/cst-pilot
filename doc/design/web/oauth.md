# OAuth 实现

本页定义 Web 对 Pi 登录交互的接线。行为见[登录规格](../../web/SPEC/auth.md)，OA 设备码协议见[认证交接](../../auth/handover.md)。

## Pi 的职责

项目锁定 Pi 0.85.1。`ModelRuntime.login(provider, "oauth", interaction)` 调用供应商登录，完成凭据保存与模型状态刷新。项目不替换登录协议，不修改安装包或固定回调地址。

| 服务 | 原生登录方式 |
|---|---|
| Anthropic | PKCE，本机 `localhost:53692/callback`；校验 state，随后交换令牌 |
| OpenAI Codex | 浏览器 PKCE，`localhost:1455/auth/callback`；另提供设备码方式 |
| OpenRouter | 随机回环端口，接收回调后交换密钥 |
| Radius | 浏览器 PKCE，`127.0.0.1:1456/oauth/callback`；另提供设备码方式 |
| GitHub Copilot、Kimi Coding、xAI、CSTOA | 设备码由后端轮询；按服务要求继续交互 |

原生代码位置：`pi-ai/dist/auth/oauth/anthropic.js`、`openai-codex.js`、`openrouter.js`、`device-code.js`。Pi 包位于 `agent/node_modules/@earendil-works/pi-coding-agent/node_modules/@earendil-works/`。

## API KEY 的原生步骤

API KEY 与 OAuth 共用交互流程管理，认证类型由 `authType` 区分。`ModelRuntime.login(provider, "api_key", interaction)` 请求的秘密、文本和选择步骤直接转成页面字段；预填密钥仅回答第一次秘密请求，其余步骤由用户填写。Cloudflare 账号和网关 ID、Bedrock 与 Vertex 登录方式不由 Web 重新定义。

`api-key/start|status|respond|cancel` 使用与 OAuth 相同的流程 ID 和版本校验。普通文本允许提交空值以支持 Pi 的确认步骤；秘密和选择仍校验必填。同一 Provider 切换类型时取消旧流程，自定义地址在整个登录流程结束后提交或回滚。

## Web 状态接线

`server/api/oauth-flow.ts` 管理流程。每个 Provider 一条进行中流程，保存 AbortController、当前问题与递增版本；凭据保持在 Pi 后端。

| Pi 交互 | Web 状态 |
|---|---|
| `auth_url` | 授权链接 |
| `device_code` | 验证地址、授权码、轮询间隔与剩余时间 |
| `prompt` | 带 ID 的文本、密钥、手动授权或选项问题 |
| `progress` / `info` | 状态与服务提供的辅助链接 |
| login 完成 | `succeeded`，发布凭据变化事件 |
| 抛错或取消 | `failed` / `cancelled`，清理问题与授权链接 |

全局事件流发布同一份状态；前端每两秒轮询补偿丢失事件。状态只按流程 ID 和版本接受，迟到结果不恢复旧问题。API 写操作采用同源校验、步骤校验和去重键。

本机回调与手动输入并行等待。收到有效回调后，Pi 交换令牌并取消备用输入。Web 不把回调页的 HTTP 成功当作最终登录成功。

## 授权窗口

`auth/oauth-window.ts` 在点击时同步创建空白授权窗，断开 opener 后跳转到服务地址。只接受 HTTP(S)，不改写 redirect_uri 或 state。

`useOAuth` 接收到后端成功状态后，页面关闭自己持有的授权窗，尝试聚焦原页，再由表单导航到允许的来源页。授权窗被拦截时保留普通链接行为；供应商跨源隔离切断引用时，原页仍完成更新，外部页可能需要手动关闭。

授权 URL 的回调参数为回环 HTTP 地址时，手动输入收在 details 中。其他服务需要的输入直接显示；账号或组织选择保持服务语义。授权链接和字段已表达操作，不显示重复说明。

## 安全与清理

- 不通过 opener、postMessage 或浏览器存储传递授权码与令牌。
- 取消、方式切换、页面离开、退出服务和 Web 关闭均取消待授权流程；保留已有凭据。
- 按 Pi 默认回环配置使用回调；浏览器与后端在同一台电脑。固定端口占用时采用 Pi 自身的失败或备用规则，不接管其他进程。
- 测试隔离 home，令牌端点使用模拟响应；测试禁止外网请求。

## 验证

| 测试 | 覆盖 |
|---|---|
| `web/test/native-oauth.test.ts` | 原生 Anthropic、Codex、OpenRouter 的真实回环服务、state 校验、凭据保存与取消；令牌端点模拟 |
| `web/test/oauth-flows.test.ts` | 多步骤输入、旧流程冲突、取消、退出与凭据替换 |
| `app/auth/oauth-window.test.ts` | opener 隔离、窗口关闭、弹窗拦截与本机回调识别 |

真实 OA 扫码、供应商账号授权、浏览器跨源隔离和系统弹窗策略仍需现场联调。
