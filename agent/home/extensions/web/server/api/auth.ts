import type { IncomingMessage, ServerResponse } from "node:http";
import type { SessionEvents } from "../session/events.ts";
import { InboxConflict } from "../session/inbox.ts";
import type { WebSessionPool } from "../session/sessions.ts";
import { getCstoaProfile } from "./account.ts";
import { OAuthFlows } from "./oauth-flow.ts";
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
	const oauthFlows = new OAuthFlows((providerId, view) => {
		globalEvents.publish("state", { type: "oauth_state", providerId, ...view });
		if (view.deviceCode) globalEvents.publish("state", { type: "oauth_device_code", providerId, ...view.deviceCode });
		if (view.state === "succeeded") {
			invalidAuth.delete(providerId);
			globalEvents.publish("state", { type: "oauth_result", providerId, ok: true });
			globalEvents.publish("state", { type: "auth_changed", providerId });
		}
	});

	const handle = async (
		request: IncomingMessage,
		response: ServerResponse,
		pathname: string,
		method: string,
	): Promise<boolean> => {
		if (pathname === "/api/auth" && method === "GET") {
			const { modelRuntime } = await pool.refreshServices();
			const credentials = await modelRuntime.listCredentials();
			send(response, 200, {
				providers: modelRuntime.getProviders().map((provider) => ({
					id: provider.id,
					name: provider.name,
					supportsApiKey: !!provider.auth.apiKey?.login,
					supportsOAuth: !!provider.auth.oauth,
					type: credentials.find((credential) => credential.providerId === provider.id)?.type ?? null,
					requiresLogin: invalidAuth.has(provider.id),
				})),
			});
			return true;
		}
		if (pathname === "/api/account" && method === "GET") {
			// 与 /api/auth 同源：刷新后再取服务，保证新注册的 provider 可见。
			const { modelRuntime } = await pool.refreshServices();
			const credentials = await modelRuntime.listCredentials();
			const signedIn = credentials.some(
				(credential) => credential.providerId === "cstoa" && credential.type === "oauth",
			);
			// 学号、姓名与额度来自 OA 资料接口；取不到时保持占位，不伪造数值。
			const cstoa = await getCstoaProfile("cstoa", modelRuntime, fetch, agentDir);
			send(response, 200, {
				providerId: "cstoa",
				signedIn,
				requiresLogin: signedIn && (invalidAuth.has("cstoa") || cstoa.expired),
				profile: { supported: cstoa.supported, studentId: cstoa.studentId, name: cstoa.name },
				quota: { supported: cstoa.balanceSupported, balance: cstoa.balance },
				reason: cstoa.reason,
			});
			return true;
		}
		const oauth = /^\/api\/auth\/([a-zA-Z0-9_-]{1,128})\/(oauth|api-key)\/(start|status|cancel|respond)$/.exec(
			pathname,
		);
		if (oauth) {
			const [, providerId, channel, action] = oauth;
			const type = channel === "oauth" ? "oauth" : "api_key";
			const { modelRuntime } = await pool.getServices();
			const capability = modelRuntime.getProvider(providerId)?.auth;
			if (type === "oauth" ? !capability?.oauth : !capability?.apiKey?.login)
				throw new Error("请求的模型服务不支持此登录方式");
			if (action === "status" && method === "GET") {
				const state = oauthFlows.status(providerId);
				send(response, 200, state.state !== "idle" && state.authType !== type ? { state: "idle" } : state);
				return true;
			}
			if (method !== "POST") return false;
			const input = await body(request);
			if (action === "start") {
				if (
					(type === "oauth" && Object.keys(input).length) ||
					Object.keys(input).some((key) => !["key", "baseUrl"].includes(key)) ||
					(input.key !== undefined &&
						(typeof input.key !== "string" || !input.key.trim() || input.key.length > 8192)) ||
					(input.baseUrl !== undefined && typeof input.baseUrl !== "string")
				)
					throw new Error("请求的登录参数无效");
				const result = await writeOnce(request, pathname, input, () =>
					serialize(`auth:${providerId}`, async () => {
						const previous = oauthFlows.status(providerId);
						if (previous.state === "pending" && previous.authType !== type)
							await oauthFlows.cancelAndWait(providerId);
						const flow = oauthFlows.start(modelRuntime, providerId, {
							type,
							key: typeof input.key === "string" ? input.key : undefined,
							wrap:
								typeof input.baseUrl === "string"
									? (login) =>
											serialize("auth-config:all", async () => {
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
									: undefined,
						});
						return { started: true, ...(await oauthFlows.ready(providerId, flow)) };
					}),
				);
				send(response, 202, result);
				return true;
			}
			if (action === "cancel") {
				if (
					Object.keys(input).some((key) => key !== "flowId") ||
					(input.flowId !== undefined && typeof input.flowId !== "string")
				)
					throw new Error("请求格式无效");
				const result = await writeOnce(request, pathname, input, () =>
					serialize(`auth:${providerId}`, async () => ({
						cancelled: oauthFlows.cancel(providerId, input.flowId as string | undefined),
					})),
				);
				send(response, 200, result);
				return true;
			}
			if (action === "respond") {
				if (
					Object.keys(input).some((key) => !["flowId", "promptId", "value"].includes(key)) ||
					typeof input.flowId !== "string" ||
					typeof input.promptId !== "string" ||
					typeof input.value !== "string"
				)
					throw new Error("请求的授权回答无效");
				const result = await writeOnce(request, pathname, input, () =>
					serialize(`auth:${providerId}`, async () => {
						oauthFlows.respond(
							providerId,
							input.flowId as string,
							input.promptId as string,
							input.value as string,
						);
						return { accepted: true };
					}),
				);
				send(response, 200, result);
				return true;
			}
			return false;
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
						await oauthFlows.cancelAndWait(providerId);
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
					await oauthFlows.cancelAndWait(providerId);
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
	return Object.assign(handle, { close: () => oauthFlows.close() });
}
