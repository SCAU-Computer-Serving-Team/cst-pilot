import type { ProviderStatus } from "../data/api";

export const CSTOA_PROVIDER = "cstoa";
export type LoginMethod = "oauth" | "api_key";
export type LoginTab = "cstoa" | LoginMethod;
export const loginTabs: { id: LoginTab; label: string }[] = [
	{ id: "cstoa", label: "cstoa" },
	{ id: "oauth", label: "OAuth" },
	{ id: "api_key", label: "APIKEY" },
];

export function tabProviders(providers: ProviderStatus[], tab: LoginTab): ProviderStatus[] {
	return orderedProviders(providers).filter((provider) =>
		tab === "cstoa"
			? provider.id === CSTOA_PROVIDER && provider.supportsOAuth
			: tab === "oauth"
				? provider.id !== CSTOA_PROVIDER && provider.supportsOAuth
				: provider.supportsApiKey,
	);
}

/** 切换登录方式时选对应的服务；当前服务不支持其他方式也不阻止切换。 */
export function chooseLoginTab(providers: ProviderStatus[], tab: LoginTab, currentId: string) {
	const candidates = tabProviders(providers, tab);
	return candidates.find((provider) => provider.id === currentId) ?? candidates[0];
}
export type ProviderFilter = "all" | LoginMethod;
export function orderedProviders(providers: ProviderStatus[]): ProviderStatus[] {
	return [...providers].sort((a, b) =>
		a.id === CSTOA_PROVIDER ? -1 : b.id === CSTOA_PROVIDER ? 1 : a.name.localeCompare(b.name),
	);
}
export function providerGroups(providers: ProviderStatus[], filter: ProviderFilter = "all") {
	const ordered = orderedProviders(providers).filter(
		(provider) => filter === "all" || loginMethods(provider).includes(filter),
	);
	const prominent = (provider: ProviderStatus) =>
		provider.id === CSTOA_PROVIDER || provider.supportsOAuth || !!provider.type || provider.requiresLogin;
	return { primary: ordered.filter(prominent), others: ordered.filter((provider) => !prominent(provider)) };
}
export function loginMethods(provider: ProviderStatus): LoginMethod[] {
	return [
		...(provider.supportsOAuth || provider.type === "oauth" ? ["oauth" as const] : []),
		...(provider.supportsApiKey || provider.type === "api_key" ? ["api_key" as const] : []),
	];
}
export function accountProvider(providers: ProviderStatus[]): ProviderStatus | undefined {
	return providers.find((provider) => provider.id === CSTOA_PROVIDER);
}
export function returnPath(value: string | null): string {
	return value && ["/", "/account", "/settings", "/settings#accounts"].includes(value) ? value : "/";
}
export function loginPath(providerId: string, method: LoginMethod, destination = "/settings#accounts"): string {
	return `/login?${new URLSearchParams({ provider: providerId, method, returnTo: returnPath(destination) })}`;
}
export function configurationPath(providerId: string, method: LoginMethod): string {
	return `/settings/provider?${new URLSearchParams({ provider: providerId, method, returnTo: "/settings#accounts" })}`;
}
export function initialLogin(providers: ProviderStatus[], requestedId: string | null, requestedMethod: string | null) {
	const available = orderedProviders(providers).filter(
		(provider) => provider.supportsApiKey || provider.supportsOAuth,
	);
	const provider = available.find((item) => item.id === requestedId) ?? available[0];
	if (!provider) return null;
	const method: LoginMethod =
		requestedMethod === "api_key" && provider.supportsApiKey
			? "api_key"
			: requestedMethod === "oauth" && provider.supportsOAuth
				? "oauth"
				: provider.supportsOAuth &&
						(provider.id === CSTOA_PROVIDER || provider.type === "oauth" || !provider.supportsApiKey)
					? "oauth"
					: "api_key";
	return { providerId: provider.id, method };
}
