import type { Api, Model } from "@earendil-works/pi-ai";
import type { ModelRuntime, SettingsManager } from "@earendil-works/pi-coding-agent";

export const RELEASE_DEFAULT_MODEL = { provider: "deepseek", modelId: "deepseek-flash" };

export function defaultModelReference(settings: SettingsManager) {
	return {
		provider: settings.getDefaultProvider() ?? RELEASE_DEFAULT_MODEL.provider,
		modelId: settings.getDefaultModel() ?? RELEASE_DEFAULT_MODEL.modelId,
	};
}

/** 目录缺失时保留选定身份；Web 发送前预检，TUI 沿用 Pi 原生请求错误。 */
export function explicitModel(runtime: ModelRuntime, reference: { provider: string; modelId: string }): Model<Api> {
	return (
		runtime.getModel(reference.provider, reference.modelId) ?? {
			provider: reference.provider,
			id: reference.modelId,
			name: reference.modelId,
			api: "openai-completions",
			baseUrl: "",
			reasoning: false,
			input: ["text"],
			contextWindow: 1000000,
			maxTokens: 16384,
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
		}
	);
}

export class ModelCredentialsRequired extends Error {
	readonly code = "credentials_required";
	readonly provider: string;
	constructor(provider: string) {
		super(`尚未配置 ${provider} 的模型服务凭据。请通过 /login 或 Web 模型服务设置完成配置后再发送。`);
		this.provider = provider;
	}
}

export async function requireModelCredentials(
	runtime: ModelRuntime,
	model: { provider: string; id: string } | undefined,
) {
	if (!model || !runtime.getModel(model.provider, model.id)) throw new Error("请求的模型已不可用，请手动选择模型。");
	if (!(await runtime.checkAuth(model.provider))) throw new ModelCredentialsRequired(model.provider);
}
