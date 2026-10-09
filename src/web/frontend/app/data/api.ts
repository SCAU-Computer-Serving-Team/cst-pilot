export type ProviderStatus = {
	id: string;
	name: string;
	supportsApiKey: boolean;
	supportsOAuth: boolean;
	type: string | null;
	requiresLogin: boolean;
};

export type OauthDeviceCode = {
	userCode: string;
	verificationUri: string;
	intervalSeconds: number;
	expiresInSeconds: number;
};

export type OauthPrompt = { id: string; message: string; placeholder?: string } & (
	| { type: "text" | "secret" | "manual_code" }
	| { type: "select"; options: { id: string; label: string; description?: string }[] }
);
export type OauthStatus = {
	authType?: "oauth" | "api_key";
	state: "idle" | "pending" | "succeeded" | "failed" | "cancelled";
	flowId?: string;
	revision?: number;
	deviceCode?: OauthDeviceCode;
	authUrl?: { url: string; instructions?: string };
	prompt?: OauthPrompt;
	message?: string;
	links?: { url: string; label?: string }[];
	error?: string;
};
export type AccountStatus = {
	providerId: "cstoa";
	signedIn: boolean;
	requiresLogin: boolean;
	profile: { supported: boolean; studentId: string | null; name: string | null; reason?: string };
	quota: { supported: boolean; balance: number | null };
};

/** 会话接口路径：id 统一编码，suffix 以 `/` 开头。 */
export const sessionPath = (id: string, suffix = "") => `/api/sessions/${encodeURIComponent(id)}${suffix}`;

type ApiError = { error?: { message?: string } };

export async function apiJson<T>(
	path: string,
	options: {
		method?: "GET" | "PUT" | "POST" | "PATCH" | "DELETE";
		body?: unknown;
		signal?: AbortSignal;
		idempotencyKey?: string;
	} = {},
): Promise<T> {
	const method = options.method ?? "GET";
	let response: Response;
	try {
		response = await fetch(path, {
			method,
			cache: "no-store",
			credentials: "same-origin",
			signal: options.signal,
			headers:
				method === "GET"
					? undefined
					: {
							"Content-Type": "application/json",
							"X-CST-Web-Request": "1",
							"Idempotency-Key": options.idempotencyKey ?? crypto.randomUUID(),
						},
			body: method === "GET" ? undefined : JSON.stringify(options.body ?? {}),
		});
	} catch (error) {
		if (options.signal?.aborted) throw error;
		throw new Error("无法连接 CST Pilot。请确认 pi 仍在运行，再重试。", { cause: error });
	}
	const data: unknown = await response.json().catch(() => null);
	if (!response.ok) {
		const message = (data as ApiError | null)?.error?.message;
		throw new Error(typeof message === "string" ? message : `请求未完成（${response.status}），请重试。`);
	}
	return data as T;
}

export function startOauthLogin(
	providerId: string,
	method: "oauth" | "api_key" = "oauth",
	body: { key?: string; baseUrl?: string } = {},
): Promise<OauthStatus & { started: boolean }> {
	return apiJson<OauthStatus & { started: boolean }>(
		`/api/auth/${encodeURIComponent(providerId)}/${method === "oauth" ? "oauth" : "api-key"}/start`,
		{
			method: "POST",
			body,
		},
	);
}

export function getOauthStatus(providerId: string, method: "oauth" | "api_key" = "oauth"): Promise<OauthStatus> {
	return apiJson<OauthStatus>(
		`/api/auth/${encodeURIComponent(providerId)}/${method === "oauth" ? "oauth" : "api-key"}/status`,
	);
}

export function cancelOauthLogin(
	providerId: string,
	flowId?: string,
	method: "oauth" | "api_key" = "oauth",
): Promise<{ cancelled: boolean }> {
	return apiJson<{ cancelled: boolean }>(
		`/api/auth/${encodeURIComponent(providerId)}/${method === "oauth" ? "oauth" : "api-key"}/cancel`,
		{
			method: "POST",
			body: flowId ? { flowId } : {},
		},
	);
}

export function respondOauthLogin(
	providerId: string,
	flowId: string,
	promptId: string,
	value: string,
	method: "oauth" | "api_key" = "oauth",
) {
	return apiJson<{ accepted: boolean }>(
		`/api/auth/${encodeURIComponent(providerId)}/${method === "oauth" ? "oauth" : "api-key"}/respond`,
		{
			method: "POST",
			body: { flowId, promptId, value },
		},
	);
}
