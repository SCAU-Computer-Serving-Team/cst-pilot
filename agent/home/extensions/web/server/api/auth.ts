import type { IncomingMessage, ServerResponse } from "node:http";
import type { ModelRuntime } from "@earendil-works/pi-coding-agent";
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

type OauthFlowState = "pending" | "succeeded" | "failed" | "cancelled";

interface OauthFlow {
	controller: AbortController;
	state: OauthFlowState;
	deviceCode?: {
		userCode: string;
		verificationUri: string;
		intervalSeconds: number;
		expiresInSeconds: number;
		expiresAt: number;
	};
	error?: string;
}

function deviceCodeView(flow: OauthFlow): Omit<NonNullable<OauthFlow["deviceCode"]>, "expiresAt"> | undefined {
	if (!flow.deviceCode) return undefined;
	const { expiresAt, ...value } = flow.deviceCode;
	return {
		...value,
		expiresInSeconds: Math.max(0, Math.ceil((expiresAt - Date.now()) / 1000)),
	};
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
	/** 每个 provider 至多一条进行中的扫码流程；发起方同样是内核的 modelRuntime.login。 */
	const oauthFlows = new Map<string, OauthFlow>();

	function startOauthFlow(modelRuntime: ModelRuntime, providerId: string): OauthFlow {
		oauthFlows.get(providerId)?.controller.abort();
		const controller = new AbortController();
		const flow: OauthFlow = { controller, state: "pending" };
		oauthFlows.set(providerId, flow);
		const current = () => oauthFlows.get(providerId) === flow;
		void modelRuntime
			.login(providerId, "oauth", {
				signal: controller.signal,
				prompt: async () => {
					throw new Error("扫码登录暂不支持交互输入");
				},
				notify: (event) => {
					if (!current()) return;
					if (event.type === "device_code") {
						const expiresInSeconds = event.expiresInSeconds ?? 300;
						flow.deviceCode = {
							userCode: event.userCode,
							verificationUri: event.verificationUri,
							intervalSeconds: event.intervalSeconds ?? 5,
							expiresInSeconds,
							expiresAt: Date.now() + expiresInSeconds * 1000,
						};
						globalEvents.publish("state", { type: "oauth_device_code", providerId, ...flow.deviceCode });
					} else if (event.type === "progress") {
						globalEvents.publish("state", { type: "oauth_progress", providerId, message: event.message });
					}
				},
			})
			.then(() => {
				if (!current()) return;
				flow.state = "succeeded";
				invalidAuth.delete(providerId);
				globalEvents.publish("state", { type: "oauth_result", providerId, ok: true });
				globalEvents.publish("state", { type: "auth_changed", providerId });
			})
			.catch((error: unknown) => {
				if (!current()) return;
				if (controller.signal.aborted) {
					flow.state = "cancelled";
					globalEvents.publish("state", { type: "oauth_result", providerId, ok: false, cancelled: true });
					return;
				}
				flow.state = "failed";
				flow.error = error instanceof Error ? error.message : String(error);
				globalEvents.publish("state", { type: "oauth_result", providerId, ok: false, error: flow.error });
			});
		return flow;
	}

	async function waitForDeviceCode(flow: OauthFlow) {
		const deadline = Date.now() + 10_000;
		while (flow.state === "pending" && !flow.deviceCode) {
			if (Date.now() >= deadline) throw new Error("获取二维码超时，请重试。");
			await new Promise((resolve) => setTimeout(resolve, 50));
		}
		const value = deviceCodeView(flow);
		if (value) return value;
		if (flow.state === "cancelled") throw new Error("扫码登录已取消。");
		throw new Error(flow.error || "扫码登录未完成，请重试。");
	}

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
					supportsOAuth: !!provider.auth.oauth,
					type: credentials.find((credential) => credential.providerId === provider.id)?.type ?? null,
					requiresLogin: invalidAuth.has(provider.id),
				})),
			});
			return true;
		}
		const oauth = /^\/api\/auth\/([a-zA-Z0-9_-]{1,128})\/oauth(?:\/(start|status|cancel))?$/.exec(pathname);
		if (oauth) {
			const providerId = oauth[1];
			const action = oauth[2] ?? "";
			const { modelRuntime } = await pool.getServices();
			const authProvider = modelRuntime.getProvider(providerId);
			if (!authProvider?.auth.oauth) throw new Error("请求的模型服务不支持扫码登录");
			if (action === "start" && method === "POST") {
				const existing = oauthFlows.get(providerId);
				const flow = existing?.state === "pending" ? existing : startOauthFlow(modelRuntime, providerId);
				send(response, 202, { started: true, deviceCode: await waitForDeviceCode(flow) });
				return true;
			}
			if (action === "status" && method === "GET") {
				const flow = oauthFlows.get(providerId);
				if (!flow) {
					send(response, 200, { state: "idle" });
					return true;
				}
				send(response, 200, {
					state: flow.state,
					deviceCode: deviceCodeView(flow),
					error: flow.error,
				});
				return true;
			}
			if (action === "cancel" && method === "POST") {
				const flow = oauthFlows.get(providerId);
				if (!flow || flow.state !== "pending") {
					send(response, 200, { cancelled: false });
					return true;
				}
				flow.controller.abort();
				send(response, 200, { cancelled: true });
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
