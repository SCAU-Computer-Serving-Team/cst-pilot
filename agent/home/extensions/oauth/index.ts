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
 * 参数按 pi-ai 内置目录（providers/data）中同名或同族模型预置：
 * - deepseek-v4.1-flash 参考 deepseek-v4-flash：1M 上下文、thinking low/high/max；
 * - glm-5.3-flash 参考 zai 目录同名模型：1M 上下文、支持图片、thinking low/high/max；
 * - omen-alpha / space-bunny-free 公开无资料，保持纯文本保守值。
 * cost 全 0：真实计费与额度在 OA 侧，pi 不重复计价。
 * 思考参数走通用 reasoning_effort（未指定 thinkingFormat），由网关透传到上游。
 */
const MODELS: NonNullable<ProviderConfig["models"]> = [
	{
		id: "deepseek-v4.1-flash",
		name: "DeepSeek V4.1 Flash",
		api: "openai-completions",
		input: ["text"],
		cost: ZERO_COST,
		reasoning: true,
		thinkingLevelMap: { minimal: null, low: "low", medium: null, high: "high", max: "max" },
		contextWindow: 1_000_000,
		maxTokens: 384_000,
		compat: { maxTokensField: "max_tokens", supportsReasoningEffort: true },
	},
	{
		id: "glm-5.3-flash",
		name: "GLM-5.3 Flash",
		api: "openai-completions",
		input: ["text", "image"],
		cost: ZERO_COST,
		reasoning: true,
		thinkingLevelMap: { off: null, minimal: null, low: "low", medium: null, high: "high", xhigh: null, max: "max" },
		contextWindow: 1_000_000,
		maxTokens: 131_072,
		compat: { maxTokensField: "max_tokens", supportsReasoningEffort: true },
	},
	{
		id: "omen-alpha",
		name: "Omen Alpha",
		api: "openai-completions",
		input: ["text"],
		cost: ZERO_COST,
		reasoning: false,
		contextWindow: 131_072,
		maxTokens: 16_384,
		compat: { maxTokensField: "max_tokens", supportsReasoningEffort: false },
	},
	{
		id: "space-bunny-free",
		name: "Space Bunny Free",
		api: "openai-completions",
		input: ["text"],
		cost: ZERO_COST,
		reasoning: false,
		contextWindow: 131_072,
		maxTokens: 16_384,
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
