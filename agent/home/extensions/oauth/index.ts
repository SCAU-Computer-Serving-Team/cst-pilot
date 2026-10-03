/**
 * OA 登录扩展 · 注册 provider `cstoa`。
 *
 * 规格：doc/auth/pi-extension.md。模型请求走 OA 的 OpenAI 兼容代理
 * （pi → OA → New API），网关密钥不下发；`oauth.getApiKey` 返回设备访问令牌。
 * TUI 与 Web 共用 oa.ts 的同一个 login()：设备码展示由各 UI 形态负责。
 */

import type { ExtensionAPI, ProviderConfig } from "@earendil-works/pi-coding-agent";
import { getAgentDir } from "@earendil-works/pi-coding-agent";
import { login, refresh } from "./oa.ts";

export const PROVIDER_ID = "cstoa";
export const PROVIDER_NAME = "CSTOA OA";
const BASE_URL = "https://www.cstoa.top/api/agent/llm/v1";

const ZERO_COST = { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 };

/**
 * OA 网关（New API 分组 default）当前启用的模型，2026-10-03 取自生产 abilities。
 *
 * - cost 全 0：真实计费与额度在 OA 侧，pi 不重复计价；
 * - reasoning 全部关闭：各模型经网关转发时的思考参数尚未逐一验证，
 *   先保证纯文本路径可用，待 OA 提供权威能力表后再按模型开启；
 * - contextWindow / maxTokens 为保守初值，同样待权威表校准。
 */
const MODELS: NonNullable<ProviderConfig["models"]> = [
	{
		id: "deepseek-v4.1-flash",
		name: "DeepSeek V4.1 Flash",
		api: "openai-completions",
		input: ["text"],
		cost: ZERO_COST,
		reasoning: false,
		contextWindow: 128_000,
		maxTokens: 8_192,
		compat: { maxTokensField: "max_tokens", supportsReasoningEffort: false },
	},
	{
		id: "glm-5.3-flash",
		name: "GLM-5.3 Flash",
		api: "openai-completions",
		input: ["text"],
		cost: ZERO_COST,
		reasoning: false,
		contextWindow: 128_000,
		maxTokens: 8_192,
		compat: { maxTokensField: "max_tokens", supportsReasoningEffort: false },
	},
	{
		id: "omen-alpha",
		name: "Omen Alpha",
		api: "openai-completions",
		input: ["text"],
		cost: ZERO_COST,
		reasoning: false,
		contextWindow: 128_000,
		maxTokens: 8_192,
		compat: { maxTokensField: "max_tokens", supportsReasoningEffort: false },
	},
	{
		id: "space-bunny-free",
		name: "Space Bunny Free",
		api: "openai-completions",
		input: ["text"],
		cost: ZERO_COST,
		reasoning: false,
		contextWindow: 128_000,
		maxTokens: 8_192,
		compat: { maxTokensField: "max_tokens", supportsReasoningEffort: false },
	},
];

export default function oauth(pi: ExtensionAPI): void {
	const agentDir = getAgentDir();
	pi.registerProvider(PROVIDER_ID, {
		name: PROVIDER_NAME,
		baseUrl: BASE_URL,
		api: "openai-completions",
		oauth: {
			name: PROVIDER_NAME,
			login: (callbacks) => login(agentDir, callbacks),
			refreshToken: (credentials, signal) => refresh(credentials, signal),
			getApiKey: (credentials) => credentials.access,
		},
		models: MODELS,
	});
}
