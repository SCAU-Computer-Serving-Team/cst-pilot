/**
 * OA 登录扩展 · 注册 provider（骨架，待实现）。
 *
 * 规格：doc/auth/pi-extension.md。注册方式：
 *
 *   pi.registerProvider("cstoa", { oauth })
 *
 * oauth 对象要提供 login(callbacks) 与 refreshToken(credentials, signal)，
 * 两个函数实现在 oa.ts。字段约定见规格「模型与凭据取用」：
 * baseUrl 指向 OA 代理，api 为 openai-completions，oauth.getApiKey 返回 credentials.access。
 */
