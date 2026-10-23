import type { Credential, Provider } from "@earendil-works/pi-ai";
import { builtinProviders } from "@earendil-works/pi-ai/providers/all";
import { CstoaModels, modelBaseUrl, PROVIDER_ID, PROVIDER_NAME } from "./models.ts";
import { login, oaHost, refresh } from "./oa.ts";

/** 双登录方式共享同一 Provider；Pi 负责凭据保存、刷新互斥和模型持久化。 */
export function cstoaProvider(agentDir: string): Provider {
	const providers = builtinProviders();
	const completions = providers.find((provider) => provider.id === "deepseek");
	if (!completions) throw new Error("Pi 缺少 OpenAI Chat Completions 适配器。");
	const catalog = new CstoaModels(providers.flatMap((provider) => [...provider.getModels()]));
	async function prepare(credential: Credential, signal: AbortSignal, notify: (message: string) => void) {
		try {
			await catalog.prepare(credential, signal);
		} catch {
			signal.throwIfAborted();
			notify("登录凭据已取得，模型清单暂时无法更新，稍后将自动重试。");
		}
	}
	return {
		...completions,
		id: PROVIDER_ID,
		name: PROVIDER_NAME,
		baseUrl: modelBaseUrl("oauth"),
		auth: {
			oauth: {
				name: PROVIDER_NAME,
				login: async (interaction) => {
					const credential = {
						type: "oauth" as const,
						...(await login(agentDir, {
							signal: interaction.signal,
							onDeviceCode: (info) => interaction.notify({ type: "device_code", ...info }),
						})),
					};
					await prepare(credential, interaction.signal, (message) =>
						interaction.notify({ type: "info", message }),
					);
					return credential;
				},
				refresh: async (credential, signal) => ({ ...(await refresh(credential, signal)), type: "oauth" }),
				toAuth: async (credential) => ({ apiKey: credential.access, baseUrl: modelBaseUrl("oauth") }),
			},
			apiKey: {
				name: "CSTOA APIKEY",
				login: async (interaction) => {
					const key = (await interaction.prompt({ type: "secret", message: "输入 CSTOA APIKEY" })).trim();
					if (!key || key.length > 8192) throw new Error("CSTOA APIKEY 格式无效。");
					// 核对成员归属；额度用尽仍允许登录，网关故障不伪装为密钥无效。
					let response: Response;
					try {
						response = await fetch(`${oaHost()}/api/agent/key-profile`, {
							headers: { Authorization: `Bearer ${key}`, Accept: "application/json" },
							signal: AbortSignal.any([interaction.signal, AbortSignal.timeout(8_000)]),
							redirect: "error",
						});
					} catch {
						interaction.signal.throwIfAborted();
						throw new Error("无法连接 CSTOA，请检查网络后重试。");
					}
					const value = await response.json().catch(() => null);
					if (response.status === 401 || response.status === 403 || value?.code === 19008)
						throw new Error("CSTOA APIKEY 无效、已过期或已撤销。");
					if (!response.ok || value?.result !== true || typeof value.data?.memberId !== "string")
						throw new Error("CSTOA 账号服务暂不可用，请稍后重试。");
					const credential: Credential = { type: "api_key", key };
					await prepare(credential, interaction.signal, (message) =>
						interaction.notify({ type: "info", message }),
					);
					return credential;
				},
				resolve: async ({ credential }) =>
					credential?.key
						? { auth: { apiKey: credential.key, baseUrl: modelBaseUrl("api_key") }, source: "APIKEY" }
						: undefined,
			},
		},
		getModels: () => catalog.getModels(),
		filterModels: (models, credential) => catalog.filter(models, credential),
		refreshModels: (context) => catalog.refresh(context),
	};
}
