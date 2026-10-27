import { loginMethods, orderedProviders } from "../auth/providers.ts";
import type { ProviderStatus } from "../data/api";

export function settingsProviders(providers: ProviderStatus[], query = "") {
	const needle = query.trim().toLocaleLowerCase();
	const available = orderedProviders(providers).filter(
		(provider) =>
			provider.supportsApiKey ||
			provider.supportsOAuth ||
			provider.type ||
			provider.requiresLogin ||
			provider.custom,
	);
	return {
		configured: available.filter((provider) => provider.type || provider.requiresLogin || provider.configured),
		available: available.filter(
			(provider) =>
				!provider.type &&
				!provider.requiresLogin &&
				!provider.configured &&
				`${provider.name} ${provider.id}`.toLocaleLowerCase().includes(needle),
		),
	};
}
export function providerDescription(provider: ProviderStatus) {
	if (provider.requiresLogin) return "凭据已失效 · 需要重新登录";
	if (provider.type === "oauth") return "账号授权 · 已登录";
	if (provider.custom) return provider.configured ? "自定义服务 · 已配置" : "自定义服务 · 尚未连接";
	if (provider.type === "api_key") return "API key · 已配置";
	if (provider.configured) return "配置文件 · 已连接";
	return loginMethods(provider)
		.map((method) => (method === "oauth" ? "OAuth" : "API key"))
		.join(" · ");
}
