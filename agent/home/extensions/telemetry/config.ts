/**
 * 配置读取：agent/home/telemetry.json 与扩展目录下的 currency.json。
 * 全部被动读取，读不到就按禁用或空映射处理，不抛错。
 */

import { readFile } from "node:fs/promises";
import { join } from "node:path";

export interface TelemetryConfig {
	enabled: boolean;
	endpoint: string;
	authProvider: string;
}

export type CurrencyMap = Record<string, "CNY" | "USD">;

export async function loadConfig(agentDir: string): Promise<TelemetryConfig | undefined> {
	try {
		const raw = JSON.parse(await readFile(join(agentDir, "telemetry.json"), "utf8")) as Record<string, unknown>;
		return {
			enabled: raw.enabled !== false,
			endpoint: typeof raw.endpoint === "string" ? raw.endpoint : "",
			authProvider: typeof raw.authProvider === "string" ? raw.authProvider : "cstoa",
		};
	} catch {
		return undefined;
	}
}

/** 映射表可用时返回映射；不可读或不合法时返回 undefined，此时 currency 留空。 */
export async function loadCurrency(extensionDir: string): Promise<CurrencyMap | undefined> {
	try {
		const raw = JSON.parse(await readFile(join(extensionDir, "currency.json"), "utf8")) as Record<string, unknown>;
		const map: CurrencyMap = {};
		for (const [provider, currency] of Object.entries(raw)) {
			if (currency === "CNY" || currency === "USD") map[provider] = currency;
		}
		return map;
	} catch {
		return undefined;
	}
}

/** 该 provider 的计费币种：映射可用但缺项按 USD，映射不可用留空。 */
export function currencyOf(currency: CurrencyMap | undefined, provider: string): string {
	if (!currency) return "";
	return currency[provider] ?? "USD";
}
