import type { ModelRuntime } from "@earendil-works/pi-coding-agent";
import { oaHost } from "../../../oauth/oa.ts";

export interface AccountStatus {
	providerId: "cstoa";
	signedIn: boolean;
	requiresLogin: boolean;
	profile: { supported: boolean; studentId: string | null; name: string | null; reason?: string };
	quota: { supported: boolean; balance: number | null };
}

function object(value: unknown): Record<string, unknown> | undefined {
	return value !== null && typeof value === "object" && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: undefined;
}

/** Only the local backend uses the Agent token; personal details are never persisted or sent to telemetry. */
export async function getCstoaAccount(
	runtime: Pick<ModelRuntime, "listCredentials" | "getAuth">,
	invalidAuth: boolean,
	fetcher: typeof fetch = fetch,
): Promise<AccountStatus> {
	const credentials = await runtime.listCredentials();
	const signedIn = credentials.some((entry) => entry.providerId === "cstoa" && entry.type === "oauth");
	const result: AccountStatus = {
		providerId: "cstoa",
		signedIn,
		requiresLogin: signedIn && invalidAuth,
		profile: { supported: true, studentId: null, name: null },
		quota: { supported: false, balance: null },
	};
	if (!signedIn || result.requiresLogin) return result;
	let access: string | undefined;
	try {
		access = (await runtime.getAuth("cstoa"))?.auth?.apiKey;
	} catch {
		result.profile.reason = "账号凭据读取失败，请重试或重新扫码登录。";
		return result;
	}
	if (!access) {
		result.requiresLogin = true;
		return result;
	}
	try {
		const response = await fetcher(`${oaHost()}/api/agent/me`, {
			headers: { Authorization: `Bearer ${access}`, Accept: "application/json" },
			signal: AbortSignal.timeout(8_000),
			redirect: "error",
		});
		if (response.status === 401 || response.status === 403) {
			result.requiresLogin = true;
			return result;
		}
		if (response.status === 404 || response.status === 405) {
			result.profile.supported = false;
			result.profile.reason = "OA 的 Agent 资料接口尚未上线。";
			return result;
		}
		if (!response.ok) {
			result.profile.reason = "OA 账号资料暂时不可用，请重试。";
			return result;
		}
		const payload = object(await response.json());
		if (payload?.result === false) {
			if (payload.code === 19008) result.requiresLogin = true;
			else result.profile.reason = "OA 账号资料查询失败，请重试。";
			return result;
		}
		const data = object(payload?.data);
		if (
			payload?.result !== true ||
			typeof data?.id !== "string" ||
			!data.id.trim() ||
			data.id.length > 64 ||
			typeof data.name !== "string" ||
			!data.name.trim() ||
			data.name.length > 256
		) {
			result.profile.reason = "OA 账号资料格式无效，请稍后重试。";
			return result;
		}
		result.profile.studentId = data.id;
		result.profile.name = data.name;
	} catch {
		result.profile.reason = "无法读取 OA 账号资料，请检查网络后重试。";
	}
	return result;
}
