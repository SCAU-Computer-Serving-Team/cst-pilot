import type { IncomingMessage, ServerResponse } from "node:http";
import type { SessionEvents } from "../session/events.ts";
import { InboxConflict } from "../session/inbox.ts";
import type { WebSessionPool } from "../session/sessions.ts";
import { withProviderEndpoint } from "./provider-endpoint.ts";

interface AuthRoutesOptions {
	pool: WebSessionPool;
	agentDir: string;
	invalidAuth: Set<string>;
	globalEvents: SessionEvents;
	body(request: IncomingMessage): Promise<Record<string, unknown>>;
	writeOnce<T>(request: IncomingMessage, pathname: string, input: unknown, action: () => Promise<T>): Promise<T>;
	serialize<T>(id: string, action: () => Promise<T>): Promise<T>;
	send(response: ServerResponse, status: number, body: unknown): void;
}

/** Credential management stays within the API's origin and idempotency checks. */
export function createAuthRoutes({
	pool,
	agentDir,
	invalidAuth,
	globalEvents,
	body,
	writeOnce,
	serialize,
	send,
}: AuthRoutesOptions) {
	return async (
		request: IncomingMessage,
		response: ServerResponse,
		pathname: string,
		method: string,
	): Promise<boolean> => {
		if (pathname === "/api/auth" && method === "GET") {
			const { modelRuntime } = await pool.getServices();
			const credentials = await modelRuntime.listCredentials();
			send(response, 200, {
				providers: modelRuntime.getProviders().map((provider) => ({
					id: provider.id,
					name: provider.name,
					supportsApiKey: !!provider.auth.apiKey,
					type: credentials.find((credential) => credential.providerId === provider.id)?.type ?? null,
					requiresLogin: invalidAuth.has(provider.id),
				})),
			});
			return true;
		}
		const auth = /^\/api\/auth\/([a-zA-Z0-9_-]{1,128})\/(api-key|logout)$/.exec(pathname);
		if (!auth || (method !== "PUT" && method !== "POST")) return false;
		const [, providerId, action] = auth;
		const { modelRuntime } = await pool.getServices();
		const authProvider = modelRuntime.getProvider(providerId);
		if (!authProvider) throw new Error("请求的模型服务不存在");
		if (action === "api-key" && !authProvider.auth.apiKey) throw new Error("请求的模型服务不支持 API KEY 登录");
		const input = await body(request);
		if (action === "api-key" && method === "PUT") {
			if (
				typeof input.key !== "string" ||
				!input.key.trim() ||
				input.key.length > 8_192 ||
				(input.baseUrl !== undefined && typeof input.baseUrl !== "string") ||
				Object.keys(input).some((key) => key !== "key" && key !== "baseUrl")
			)
				throw new Error("请求的密钥或 BaseURL 格式无效");
			const result = await writeOnce(request, pathname, input, () =>
				serialize(`auth:${providerId}`, async () => {
					const login = async () => {
						let prompts = 0;
						await modelRuntime.login(providerId, "api_key", {
							prompt: async (question) => {
								if (++prompts !== 1 || question.type !== "secret")
									throw new Error("该模型服务需要其他登录信息");
								return input.key as string;
							},
							notify: () => {},
						});
						return { providerId, type: "api_key" };
					};
					const responseBody =
						typeof input.baseUrl === "string"
							? await serialize("auth-config:all", async () => {
									if (
										pool
											.snapshot()
											.some(
												({ id, running }) =>
													running && pool.get(id)?.session.model?.provider === providerId,
											)
									)
										throw new InboxConflict("此 Provider 的会话正在执行，请完成后再修改 BaseURL");
									return withProviderEndpoint(
										agentDir,
										providerId,
										input.baseUrl as string,
										modelRuntime,
										login,
									);
								})
							: await login();
					invalidAuth.delete(providerId);
					globalEvents.publish("state", { type: "auth_changed", providerId });
					return responseBody;
				}),
			);
			send(response, 200, result);
			return true;
		}
		if (action === "logout" && method === "POST") {
			if (Object.keys(input).length) throw new Error("请求格式无效");
			const result = await writeOnce(request, pathname, input, () =>
				serialize(`auth:${providerId}`, async () => {
					await modelRuntime.logout(providerId);
					invalidAuth.delete(providerId);
					globalEvents.publish("state", { type: "auth_changed", providerId });
					return { providerId, type: null };
				}),
			);
			send(response, 200, result);
			return true;
		}
		return false;
	};
}
