export type ProviderQuota = {
	provider: string;
	supported: boolean;
	reason?: string;
	requiresLogin?: boolean;
	windows?: Record<string, { status: string; percent: number; resetsAt: string | null }>;
	balance?: number | null;
	currency?: string | null;
	unit?: "credit";
};

export const quotaProviders = new Set(["cstoa", "deepseek", "opencode-go"]);

export function formatQuotaBalance(quota: ProviderQuota | null): string | null {
	if (!quota?.supported || typeof quota.balance !== "number" || !Number.isFinite(quota.balance)) return null;
	if (quota.provider === "cstoa") {
		if (quota.unit !== "credit" || !Number.isSafeInteger(quota.balance) || quota.balance < 0) return null;
		return `${quota.balance.toLocaleString("zh-CN")} 额度点`;
	}
	if (quota.provider === "deepseek" && ["CNY", "USD"].includes(quota.currency ?? ""))
		return `${quota.currency === "USD" ? "US$" : "¥"} ${quota.balance.toFixed(2)}`;
	return null;
}
