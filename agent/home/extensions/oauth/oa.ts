/**
 * OA 登录扩展 · 设备授权与令牌交换（骨架，待实现）。
 *
 * 规格：doc/auth/README.md「范围与流程」「OA 接口」「登录有效期」。
 * 协议是 OAuth 2.0 设备授权流（RFC 8628）：
 *
 * 1. POST /api/oauth/device_authorization 申请设备码（按 IP 限流，无需鉴权）。
 * 2. callbacks.onDeviceCode 展示网址与数字码，按 interval 轮询 POST /api/oauth/token。
 * 3. 轮询错误用 RFC 8628 的 authorization_pending / slow_down / expired_token / access_denied。
 * 4. 成功返回 { access, refresh, expires }；默认模式 refresh 为空串。
 * 5. refreshToken(credentials, signal) 用 refresh_token 换轮换后的凭据，响应 signal 的 15 秒超时。
 *
 * 联调前 OA 端点未就绪时，可在本机起一个 mock 设备授权服务，
 * 参照遥测 e2e 的做法（E:\tmp 下的 e2e.test.ts，mock OpenAI SSE 服务同款思路）。
 */
