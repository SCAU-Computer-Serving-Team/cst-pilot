import { loginMethods, orderedProviders } from "../auth/providers.ts";
import type { ProviderStatus } from "../data/api";

export function settingsProviders(providers: ProviderStatus[], query = "") {
	const needle = query.trim().toLocaleLowerCase();
	const available = orderedProviders(providers).filter(
		(provider) => provider.supportsApiKey || provider.supportsOAuth || provider.type || provider.requiresLogin,
	);
	return {
		configured: available.filter((provider) => provider.type || provider.requiresLogin),
		available: available.filter(
			(provider) =>
				!provider.type &&
				!provider.requiresLogin &&
				`${provider.name} ${provider.id}`.toLocaleLowerCase().includes(needle),
		),
	};
}
export function providerDescription(provider: ProviderStatus) {
	if (provider.requiresLogin) return "凭据已失效 · 需要重新登录";
	if (provider.type === "oauth") return "账号授权 · 已登录";
	if (provider.type === "api_key") return "API key · 已配置";
	return loginMethods(provider)
		.map((method) => (method === "oauth" ? "OAuth" : "API key"))
		.join(" · ");
}
