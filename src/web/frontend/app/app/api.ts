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

export type OauthStatus = {
	state: "idle" | "pending" | "succeeded" | "failed" | "cancelled";
	deviceCode?: OauthDeviceCode;
	error?: string;
};

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
): Promise<{ started: boolean; deviceCode?: OauthDeviceCode }> {
	return apiJson<{ started: boolean; deviceCode?: OauthDeviceCode }>(
		`/api/auth/${encodeURIComponent(providerId)}/oauth/start`,
		{
			method: "POST",
		},
	);
}

export function getOauthStatus(providerId: string): Promise<OauthStatus> {
	return apiJson<OauthStatus>(`/api/auth/${encodeURIComponent(providerId)}/oauth/status`);
}

export function cancelOauthLogin(providerId: string): Promise<{ cancelled: boolean }> {
	return apiJson<{ cancelled: boolean }>(`/api/auth/${encodeURIComponent(providerId)}/oauth/cancel`, {
		method: "POST",
	});
}
