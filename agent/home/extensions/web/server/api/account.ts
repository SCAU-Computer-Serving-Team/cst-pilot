import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import type { ModelRuntime } from "@earendil-works/pi-coding-agent";

/**
 * cstoa 专属账号资料：学号、姓名与大模型剩余额度点。
 *
 * 规格：doc/web/SPEC/app-router.md「专属账号」。数据来自 OA 的
 * GET /api/agent/profile，用 pi 保存的 cstoa 访问令牌调用，响应遵循
 * OA 信封 {result, data}。取不到时返回 supported=false 或只给学号，
 * 由前端显示占位，不伪造数值；令牌不进入浏览器响应。
 */

export type CstoaProfile = {
	/** 是否至少拿到了学号或姓名。 */
	supported: boolean;
	studentId: string | null;
	name: string | null;
	balance: number | null;
	balanceSupported: boolean;
	expired: boolean;
	reason?: string;
};

const TTL_MS = 60_000;
const REQUEST_TIMEOUT_MS = 8_000;
const LOOPBACK = new Set(["localhost", "127.0.0.1", "[::1]"]);
const cache = new Map<string, { at: number; value: CstoaProfile }>();

function object(value: unknown): Record<string, unknown> | null {
	return value !== null && typeof value === "object" && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: null;
}

function text(value: unknown): string | null {
	return typeof value === "string" && value.trim() ? value.trim() : null;
}

function integer(value: unknown): number | null {
	return typeof value === "number" && Number.isFinite(value) ? value : null;
}

/** OA 源站：优先 CSTOA_OA_HOST（本地联调），否则取 provider baseUrl 的 origin。 */
function oaOrigin(baseUrl: string): string | null {
	const raw = process.env.CSTOA_OA_HOST?.trim() || baseUrl;
	let url: URL;
	try {
		url = new URL(raw);
	} catch {
		return null;
	}
	if (url.protocol !== "https:" && !(url.protocol === "http:" && LOOPBACK.has(url.hostname))) return null;
	if (url.username || url.password || url.search || url.hash) return null;
	return url.origin;
}

/** 令牌载荷只用于展示身份，不验签、不落盘。 */
function tokenPayload(token: string): Record<string, unknown> | null {
	const [body] = token.split(".");
	if (!body) return null;
	try {
		const json = Buffer.from(body.replace(/-/g, "+").replace(/_/g, "/"), "base64").toString("utf8");
		return object(JSON.parse(json));
	} catch {
		return null;
	}
}

/** 凭据过期且没有刷新令牌时 pi 的 getAuth 会抛错；此时直接读 auth.json，只用于展示。 */
async function storedToken(agentDir: string | undefined, providerId: string): Promise<string | undefined> {
	if (!agentDir) return undefined;
	try {
		const raw = JSON.parse(await readFile(join(agentDir, "auth.json"), "utf8")) as Record<string, unknown>;
		return text(object(raw[providerId])?.access) ?? undefined;
	} catch {
		return undefined;
	}
}

function missing(reason: string): CstoaProfile {
	return {
		supported: false,
		studentId: null,
		name: null,
		balance: null,
		balanceSupported: false,
		expired: false,
		reason,
	};
}

/** 读取专属账号资料；只联系已知 OA 源站，失败时给出可展示的降级结果。 */
export async function getCstoaProfile(
	providerId: string,
	runtime: ModelRuntime,
	fetcher: typeof fetch = fetch,
	agentDir?: string,
): Promise<CstoaProfile> {
	const model = runtime.getModels(providerId)[0];
	if (!model) return missing("该模型服务未配置");
	const origin = oaOrigin(model.baseUrl ?? "");
	if (!origin) return missing("OA 地址无效");
	let token: string | undefined;
	try {
		token = (await runtime.getAuth(model))?.auth?.apiKey;
	} catch {
		token = undefined;
	}
	token ??= await storedToken(agentDir, providerId);
	if (!token) return missing("缺少凭据");

	const claims = tokenPayload(token);
	const studentId = text(claims?.mid);
	const exp = integer(claims?.exp);
	const expired = exp !== null && exp * 1000 <= Date.now();
	if (expired)
		return {
			supported: true,
			studentId,
			name: null,
			balance: null,
			balanceSupported: false,
			expired: true,
			reason: "登录已过期，请重新登录",
		};

	const cacheKey = `${providerId}:${origin}:${createHash("sha256").update(token).digest("hex")}`;
	const cached = cache.get(cacheKey);
	if (cached && Date.now() - cached.at < TTL_MS) return cached.value;

	const base: CstoaProfile = {
		supported: Boolean(studentId),
		studentId,
		name: null,
		balance: null,
		balanceSupported: false,
		expired: false,
	};

	let value: CstoaProfile;
	try {
		const upstream = await fetcher(`${origin}/api/agent/profile`, {
			headers: { Authorization: `Bearer ${token}`, Accept: "application/json" },
			signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
			redirect: "error",
		});
		if (!upstream.ok) {
			value = { ...base, reason: `资料接口返回 ${upstream.status}` };
		} else {
			// OA 所有接口都包 {result, data}；业务失败同样是 HTTP 200。
			const envelope = object(await upstream.json());
			const data = object(envelope?.data);
			if (envelope?.result !== true || !data) {
				value = { ...base, reason: text(envelope?.reason) ?? "资料接口返回无效内容" };
			} else {
				const balance = integer(data.balance);
				const id = text(data.memberId) ?? studentId;
				value = {
					supported: Boolean(id || text(data.name)),
					studentId: id,
					name: text(data.name),
					balance,
					balanceSupported: data.balanceAvailable === true && balance !== null,
					expired: false,
				};
			}
		}
	} catch {
		value = { ...base, reason: "资料查询失败" };
	}
	cache.set(cacheKey, { at: Date.now(), value });
	return value;
}
