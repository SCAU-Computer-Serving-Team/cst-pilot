import { createHash } from "node:crypto";
import type { ModelRuntime } from "@earendil-works/pi-coding-agent";

type Quota = {
	provider: string;
	supported: boolean;
	reason?: string;
	windows?: Record<string, { status: string; percent: number; resetsAt: string | null }>;
	balance?: number | null;
	currency?: string | null;
	fetchedAt?: string;
};

const TTL_MS = 60_000;
const cache = new Map<string, { at: number; value: Quota }>();

function object(value: unknown): Record<string, unknown> | null {
	return value !== null && typeof value === "object" && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: null;
}

function quotaEndpoint(provider: string, baseUrl: string): URL | null {
	let url: URL;
	try {
		url = new URL(baseUrl);
	} catch {
		return null;
	}
	if (url.protocol !== "https:" || url.port || url.username || url.password || url.search || url.hash) return null;
	const path = url.pathname.replace(/\/+$/, "");
	if (provider === "deepseek" && url.hostname === "api.deepseek.com" && ["", "/v1"].includes(path))
		return new URL("https://api.deepseek.com/user/balance");
	if (provider === "opencode-go" && url.hostname === "opencode.ai" && ["/zen/go", "/zen/go/v1"].includes(path))
		return new URL("https://opencode.ai/zen/go/v1/usage");
	return null;
}

function parseGo(payload: unknown): Pick<Quota, "windows" | "balance" | "currency"> {
	const usage = object(object(payload)?.usage);
	if (!usage) throw new Error("额度响应格式无效");
	const windows: NonNullable<Quota["windows"]> = {};
	for (const name of ["rolling", "weekly", "monthly"]) {
		const window = object(usage[name]);
		if (
			!window ||
			typeof window.percent !== "number" ||
			!Number.isFinite(window.percent) ||
			window.percent < 0 ||
			window.percent > 100 ||
			!(["ok", "rate-limited"] as unknown[]).includes(window.status) ||
			typeof window.resetsAt !== "string" ||
			!Number.isFinite(Date.parse(window.resetsAt))
		)
			throw new Error("额度响应格式无效");
		windows[name] = { status: window.status as string, percent: window.percent, resetsAt: window.resetsAt };
	}
	return { windows, balance: null, currency: null };
}

function parseDeepSeek(payload: unknown): Pick<Quota, "balance" | "currency"> {
	const data = object(payload);
	if (!data || typeof data.is_available !== "boolean" || !Array.isArray(data.balance_infos))
		throw new Error("余额响应格式无效");
	const balances = data.balance_infos.map(object);
	if (
		balances.some(
			(item) =>
				!item ||
				!["CNY", "USD"].includes(item.currency as string) ||
				typeof item.total_balance !== "string" ||
				!/^(?:0|[1-9]\d*)(?:\.\d+)?$/.test(item.total_balance) ||
				!Number.isFinite(Number(item.total_balance)),
		)
	)
		throw new Error("余额响应格式无效");
	// Prefer CNY when the account has both currencies; never add different currencies together.
	const selected = balances.find((item) => item?.currency === "CNY") ?? balances[0];
	if (!selected) throw new Error("余额响应格式无效");
	return { balance: Number(selected.total_balance), currency: selected.currency as string };
}

/** Query only known first-party endpoints; reuse Pi's credential without exposing it to the browser. */
export async function getProviderQuota(
	provider: string,
	runtime: ModelRuntime,
	fetcher: typeof fetch = fetch,
): Promise<Quota> {
	const unsupported = (reason: string): Quota => ({ provider, supported: false, reason });
	const model = runtime.getModels(provider).find((item) => quotaEndpoint(provider, item.baseUrl));
	const endpoint = model && quotaEndpoint(provider, model.baseUrl);
	if (!endpoint || !model) return unsupported("该模型服务未提供额度查询接口");
	try {
		const key = (await runtime.getAuth(model))?.auth?.apiKey;
		if (!key) return unsupported("缺少凭据");
		const cacheKey = `${provider}:${endpoint.href}:${createHash("sha256").update(key).digest("hex")}`;
		const cached = cache.get(cacheKey);
		if (cached && Date.now() - cached.at < TTL_MS) return cached.value;
		const upstream = await fetcher(endpoint, {
			headers: { Authorization: `Bearer ${key}`, Accept: "application/json" },
			signal: AbortSignal.timeout(8_000),
			redirect: "error",
		});
		if (!upstream.ok) return unsupported(`额度接口返回 ${upstream.status}`);
		const payload: unknown = await upstream.json();
		const fields = provider === "deepseek" ? parseDeepSeek(payload) : parseGo(payload);
		const result: Quota = { provider, supported: true, ...fields, fetchedAt: new Date().toISOString() };
		cache.set(cacheKey, { at: Date.now(), value: result });
		return result;
	} catch {
		return unsupported("额度查询失败");
	}
}
