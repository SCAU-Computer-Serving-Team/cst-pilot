import { readFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

export interface TelemetryEndpoint {
	url: string;
	/** 仅用于该端点的公开 CA 证书。 */
	ca?: string;
	/** CA 配置无效时保留队列，不向该端点发送凭据。 */
	unavailable?: boolean;
}

export interface TelemetryConfig {
	enabled: boolean;
	endpoints: TelemetryEndpoint[];
	authProvider: string;
}

export type CurrencyMap = Record<string, "CNY" | "USD">;

/** 被动读取配置；单端点配置也可读取，队列迁移由 delivery 处理。 */
export async function loadConfig(
	agentDir: string,
	extensionDir = dirname(fileURLToPath(import.meta.url)),
): Promise<TelemetryConfig | undefined> {
	try {
		const raw = JSON.parse(await readFile(join(agentDir, "telemetry.json"), "utf8")) as Record<string, unknown>;
		const candidates: unknown[] = Array.isArray(raw.endpoints)
			? raw.endpoints
			: typeof raw.endpoint === "string" && raw.endpoint
				? [{ url: raw.endpoint }]
				: [];
		const endpoints: TelemetryEndpoint[] = [];
		for (const value of candidates) {
			const input = typeof value === "string" ? { url: value } : value;
			if (!input || typeof input !== "object" || !("url" in input) || typeof input.url !== "string") continue;
			let url: URL;
			try {
				url = new URL(input.url);
			} catch {
				continue;
			}
			if (
				url.username ||
				url.password ||
				url.hash ||
				!(
					url.protocol === "https:" ||
					(url.protocol === "http:" && ["127.0.0.1", "localhost", "[::1]"].includes(url.hostname))
				)
			)
				continue;
			if (endpoints.some((endpoint) => endpoint.url === url.href)) continue;
			const endpoint: TelemetryEndpoint = { url: url.href };
			if ("caFile" in input) {
				const file = input.caFile;
				if (typeof file !== "string" || !/^[a-zA-Z0-9_-]+\.crt$/.test(file) || url.protocol !== "https:") {
					endpoint.unavailable = true;
				} else {
					try {
						endpoint.ca = await readFile(join(extensionDir, file), "utf8");
						if (!endpoint.ca.includes("-----BEGIN CERTIFICATE-----")) endpoint.unavailable = true;
					} catch {
						endpoint.unavailable = true;
					}
				}
			}
			endpoints.push(endpoint);
		}
		return {
			enabled: raw.enabled !== false,
			endpoints,
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

export function currencyOf(currency: CurrencyMap | undefined, provider: string): string {
	if (!currency) return "";
	return currency[provider] ?? "USD";
}
