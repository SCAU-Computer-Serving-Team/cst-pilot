import type { IncomingMessage, ServerResponse } from "node:http";
import type { ModelRuntime } from "@earendil-works/pi-coding-agent";
import type { SessionEvents } from "../session/events.ts";
import { InboxConflict } from "../session/inbox.ts";
import type { WebSessionPool } from "../session/sessions.ts";
import type { CstoaAccount } from "./account.ts";
import {
	deleteCustomProvider,
	readCustomProvider,
	readCustomProviders,
	saveCustomProvider,
} from "./custom-provider.ts";
import { body, send } from "./http.ts";
import { OAuthFlows } from "./oauth-flow.ts";
import { withProviderEndpoint } from "./provider-endpoint.ts";

interface AuthRoutesOptions {
	pool: WebSessionPool;
	agentDir: string;
	invalidAuth: Set<string>;
	globalEvents: SessionEvents;
	account: CstoaAccount;
	writeOnce<T>(request: IncomingMessage, pathname: string, input: unknown, action: () => Promise<T>): Promise<T>;
	serialize<T>(id: string, action: () => Promise<T>): Promise<T>;
}

/** Credential management stays within the API's origin and idempotency checks. */
export function createAuthRoutes({
	pool,
	agentDir,
	invalidAuth,
	globalEvents,
	account,
	writeOnce,
	serialize,
}: AuthRoutesOptions) {
	const refreshes = new Map<string, { controller: AbortController; task: Promise<void> }>();
	let closing = false;
	function refreshProviderCatalog(providerId: string, runtime: ModelRuntime): void {
		if (closing) return;
		refreshes.get(providerId)?.controller.abort();
		const controller = new AbortController();
		const timeout = setTimeout(() => controller.abort(), 15_000);
		const task = (async () => {
			try {
				// 沿用 Pi 登录后的单 Provider 刷新；项目启动时关闭目录联网，登录动作显式允许此服务联网。
				const result = await runtime.refresh({
					providers: [providerId],
					allowNetwork: true,
					signal: controller.signal,
				});
				if (!closing && refreshes.get(providerId)?.controller === controller)
					globalEvents.publish("state", {
						type: "models_changed",
						providerId,
						...(result.aborted || result.errors.size ? { warning: "模型目录刷新失败，正在使用缓存模型。" } : {}),
					});
			} catch {
				if (!closing && refreshes.get(providerId)?.controller === controller)
					globalEvents.publish("state", {
						type: "models_changed",
						providerId,
						warning: "模型目录刷新失败，正在使用缓存模型。",
					});
			} finally {
				clearTimeout(timeout);
			}
		})();
		refreshes.set(providerId, { controller, task });
		void task.finally(() => {
			if (refreshes.get(providerId)?.task === task) refreshes.delete(providerId);
		});
	}
	const oauthFlows = new OAuthFlows((providerId, view) => {
		globalEvents.publish("state", { type: "oauth_state", providerId, ...view });
		if (view.deviceCode) globalEvents.publish("state", { type: "oauth_device_code", providerId, ...view.deviceCode });
		if (view.state === "succeeded") {
			invalidAuth.delete(providerId);
			if (providerId === "cstoa") account.invalidate();
			globalEvents.publish("state", { type: "oauth_result", providerId, ok: true });
			globalEvents.publish("state", { type: "auth_changed", providerId });
			void pool
				.getServices()
				.then(({ modelRuntime }) => refreshProviderCatalog(providerId, modelRuntime))
				.catch(() => {});
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
			const customIds = new Set(await readCustomProviders(agentDir));
			send(response, 200, {
				providers: modelRuntime.getProviders().map((provider) => ({
					custom: customIds.has(provider.id),
					configured: modelRuntime.getProviderAuthStatus(provider.id).configured,
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
		if (pathname === "/api/auth/custom" && method === "POST") {
			const input = await body(request);
			const result = await writeOnce(request, pathname, input, () =>
				serialize("auth-config:all", async () => {
					const { modelRuntime } = await pool.getServices();
					const providerId = input.id;
					if (
						typeof providerId === "string" &&
						pool
							.snapshot()
							.some(({ id, running }) => running && pool.get(id)?.session.model?.provider === providerId)
					)
						throw new InboxConflict("此模型服务的会话正在执行，请完成后再修改配置");
					pool.catalog.invalidate();
					const saved = await saveCustomProvider(agentDir, input, modelRuntime);
					globalEvents.publish("state", { type: "auth_changed", providerId: saved.id });
					globalEvents.publish("state", { type: "models_changed", providerId: saved.id });
					return saved;
				}),
			);
			send(response, 200, result);
			return true;
		}
		const customPath = /^\/api\/auth\/custom\/([a-z][a-z0-9_-]{1,63})$/.exec(pathname);
		if (customPath && method === "GET") {
			const provider = await readCustomProvider(agentDir, customPath[1]);
			if (!provider) return false;
			send(response, 200, provider);
			return true;
		}
		if (customPath && method === "DELETE") {
			const input = await body(request);
			const { revision } = input;
			if (typeof revision !== "string" || Object.keys(input).length !== 1) throw new Error("配置版本无效");
			const result = await writeOnce(request, pathname, input, () =>
				serialize("auth-config:all", async () => {
					if (
						pool
							.snapshot()
							.some(({ id, running }) => running && pool.get(id)?.session.model?.provider === customPath[1])
					)
						throw new InboxConflict("此模型服务的会话正在执行，请完成后再移除");
					const { modelRuntime } = await pool.getServices();
					pool.catalog.invalidate();
					const deleted = await deleteCustomProvider(agentDir, customPath[1], revision, modelRuntime);
					globalEvents.publish("state", { type: "auth_changed", providerId: deleted.id });
					globalEvents.publish("state", { type: "models_changed", providerId: deleted.id });
					return deleted;
				}),
			);
			send(response, 200, result);
			return true;
		}
		if (pathname === "/api/account" && method === "GET") {
			const { modelRuntime } = await pool.refreshServices();
			if (new URL(request.url ?? "/", "http://localhost").searchParams.get("refresh") === "1") account.invalidate();
			send(response, 200, await account.getAccount(modelRuntime, invalidAuth.has("cstoa")));
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
			const { key, baseUrl, flowId, promptId, value } = input;
			if (action === "start") {
				if (
					(type === "oauth" && Object.keys(input).length) ||
					Object.keys(input).some((key) => !["key", "baseUrl"].includes(key)) ||
					(key !== undefined && (typeof key !== "string" || !key.trim() || key.length > 8192)) ||
					(baseUrl !== undefined && typeof baseUrl !== "string")
				)
					throw new Error("请求的登录参数无效");
				const result = await writeOnce(request, pathname, input, () =>
					serialize(`auth:${providerId}`, async () => {
						const previous = oauthFlows.status(providerId);
						if (previous.state === "pending" && previous.authType !== type)
							await oauthFlows.cancelAndWait(providerId);
						const flow = oauthFlows.start(modelRuntime, providerId, {
							type,
							key: typeof key === "string" ? key : undefined,
							wrap:
								typeof baseUrl === "string"
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
												return withProviderEndpoint(agentDir, providerId, baseUrl, modelRuntime, login);
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
					(flowId !== undefined && typeof flowId !== "string")
				)
					throw new Error("请求格式无效");
				const result = await writeOnce(request, pathname, input, () =>
					serialize(`auth:${providerId}`, async () => ({
						cancelled: oauthFlows.cancel(providerId, flowId),
					})),
				);
				send(response, 200, result);
				return true;
			}
			if (action === "respond") {
				if (
					Object.keys(input).some((key) => !["flowId", "promptId", "value"].includes(key)) ||
					typeof flowId !== "string" ||
					typeof promptId !== "string" ||
					typeof value !== "string"
				)
					throw new Error("请求的授权回答无效");
				const result = await writeOnce(request, pathname, input, () =>
					serialize(`auth:${providerId}`, async () => {
						oauthFlows.respond(providerId, flowId, promptId, value);
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
		const { key, baseUrl } = input;
		if (action === "api-key" && method === "PUT") {
			if (
				typeof key !== "string" ||
				!key.trim() ||
				key.length > 8_192 ||
				(baseUrl !== undefined && typeof baseUrl !== "string") ||
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
								return key;
							},
							notify: () => {},
						});
						return { providerId, type: "api_key" };
					};
					const responseBody =
						typeof baseUrl === "string"
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
									return withProviderEndpoint(agentDir, providerId, baseUrl, modelRuntime, login);
								})
							: await login();
					invalidAuth.delete(providerId);
					if (providerId === "cstoa") account.invalidate();
					globalEvents.publish("state", { type: "auth_changed", providerId });
					refreshProviderCatalog(providerId, modelRuntime);
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
					if (providerId === "cstoa") account.invalidate();
					globalEvents.publish("state", { type: "auth_changed", providerId });
					return { providerId, type: null };
				}),
			);
			send(response, 200, result);
			return true;
		}
		return false;
	};
	return Object.assign(handle, {
		close: async () => {
			closing = true;
			await oauthFlows.close();
			for (const { controller } of refreshes.values()) controller.abort();
			await Promise.allSettled([...refreshes.values()].map(({ task }) => task));
		},
	});
}
