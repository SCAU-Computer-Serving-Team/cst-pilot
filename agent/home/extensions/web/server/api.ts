import { createHash } from "node:crypto";
import { rm } from "node:fs/promises";
import type { IncomingMessage, ServerResponse } from "node:http";
import { join } from "node:path";
import { resolveModelScopeWithDiagnostics } from "@earendil-works/pi-coding-agent";
import { type ImageRef, ImageStore } from "./attachments.ts";
import { SessionEvents } from "./events.ts";
import { type Delivery, InboxConflict, SessionInbox } from "./inbox.ts";
import { MutationReceipts } from "./receipts.ts";
import type { WebSessionPool } from "./sessions.ts";

function send(response: ServerResponse, status: number, body: unknown): void {
	response.writeHead(status, { "Content-Type": "application/json; charset=utf-8", "Cache-Control": "no-store" });
	response.end(JSON.stringify(body));
}
function problem(response: ServerResponse, status: number, code: string, message: string): void {
	send(response, status, { error: { code, message } });
}
async function body(request: IncomingMessage, limit = 128 * 1024): Promise<Record<string, unknown>> {
	if (!request.headers["content-type"]?.startsWith("application/json")) throw new Error("请求须为 JSON");
	let size = 0;
	const parts: Buffer[] = [];
	for await (const chunk of request) {
		size += chunk.length;
		if (size > limit) throw new Error("请求内容过大");
		parts.push(chunk);
	}
	const value: unknown = JSON.parse(Buffer.concat(parts).toString("utf8"));
	if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("请求格式无效");
	return value as Record<string, unknown>;
}

/** A deliberately narrow first API slice: unsupported operations remain 404. */
export function createWebApi(pool: WebSessionPool, agentDir: string, port: number) {
	const inboxes = new Map<string, SessionInbox>();
	const streams = new Map<string, SessionEvents>();
	const globalEvents = new SessionEvents();
	const invalidAuth = new Set<string>();
	const images = new ImageStore(join(agentDir, "web-inbox", "attachments"));
	const receipts = new MutationReceipts(join(agentDir, "web-inbox", "receipts"));
	const sessionWrites = new Map<string, Promise<void>>();
	async function serialize<T>(id: string, action: () => Promise<T>): Promise<T> {
		const previous = sessionWrites.get(id) ?? Promise.resolve();
		const task = previous.then(action);
		const tail = task.then(
			() => undefined,
			() => undefined,
		);
		sessionWrites.set(id, tail);
		try {
			return await task;
		} finally {
			if (sessionWrites.get(id) === tail) sessionWrites.delete(id);
		}
	}
	async function writeOnce<T>(
		request: IncomingMessage,
		pathname: string,
		input: unknown,
		action: () => Promise<T>,
	): Promise<T> {
		const key = request.headers["idempotency-key"];
		if (typeof key !== "string") throw new Error("缺少有效的去重键");
		const signature = createHash("sha256")
			.update(JSON.stringify([request.method, pathname, input]))
			.digest("hex");
		const sessionId =
			/^\/api\/sessions\/([a-zA-Z0-9_-]{1,128})(?:\/|$)/.exec(pathname)?.[1] ??
			(input && typeof input === "object" && "sessionId" in input && typeof input.sessionId === "string"
				? input.sessionId
				: undefined);
		return receipts.once(key, signature, () => (sessionId ? serialize(sessionId, action) : action()));
	}
	const imageContent = async (id: string, refs: ImageRef[]) =>
		Promise.all(
			refs.map(async (ref) => ({
				type: "image" as const,
				mimeType: ref.mimeType,
				data: (await images.read(id, ref.id)).toString("base64"),
			})),
		);
	const origin = `http://127.0.0.1:${port}`;
	async function inbox(id: string): Promise<SessionInbox> {
		const slot = await pool.openSaved(id);
		let instance = inboxes.get(id);
		if (!streams.has(id)) {
			const stream = new SessionEvents(slot.session);
			streams.set(id, stream);
			slot.ui.onChange((questions) => stream.publish("session", { type: "ui_requests", questions }));
		}
		if (!instance) {
			instance = new SessionInbox(
				join(agentDir, "web-inbox"),
				id,
				{
					isBusy: () => !slot.session.isIdle,
					prompt: async (text, refs, skill) => {
						const message = skill ? `/skill:${skill}${text ? ` ${text}` : ""}` : text;
						await slot.session.prompt(message, {
							source: "rpc",
							expandPromptTemplates: !!skill,
							images: await imageContent(id, refs),
						});
						const last = slot.session.messages.at(-1);
						if (last?.role === "assistant" && last.stopReason === "error") {
							if (
								slot.session.model &&
								/(?:\b401\b|unauthorized|invalid.api.key|authentication.failed)/i.test(last.errorMessage ?? "")
							) {
								invalidAuth.add(slot.session.model.provider);
								globalEvents.publish("state", {
									type: "auth_changed",
									providerId: slot.session.model.provider,
								});
							}
							throw new Error("模型调用失败");
						}
					},
					steer: async (text, refs, skill) =>
						skill
							? slot.session.prompt(`/skill:${skill}${text ? ` ${text}` : ""}`, {
									source: "rpc",
									expandPromptTemplates: true,
									streamingBehavior: "steer",
									images: await imageContent(id, refs),
								})
							: slot.session.steer(text, await imageContent(id, refs)),
				},
				(snapshot) => streams.get(id)?.publish("queue", snapshot),
			);
			inboxes.set(id, instance);
			slot.session.subscribe((event) => {
				if (event.type === "agent_settled") setTimeout(() => instance?.wake(), 0);
			});
		}
		return instance;
	}

	const handle = async (request: IncomingMessage, response: ServerResponse, pathname: string): Promise<boolean> => {
		if (pathname !== "/api" && !pathname.startsWith("/api/")) return false;
		const method = request.method ?? "GET";
		if (request.headers.origin && request.headers.origin !== origin) {
			problem(response, 403, "invalid_origin", "请求来源不受信任");
			return true;
		}
		if (
			method !== "GET" &&
			method !== "HEAD" &&
			(request.headers.origin !== origin || request.headers["x-cst-web-request"] !== "1")
		) {
			problem(response, 403, "invalid_origin", "写入请求须来自本机页面");
			return true;
		}
		try {
			if (pathname === "/api/events" && method === "GET") {
				globalEvents.serve(request, response);
				return true;
			}
			if (pathname === "/api/settings") {
				const { settingsManager } = await pool.getServices();
				if (method === "GET") {
					send(response, 200, { theme: settingsManager.getThemeSetting() ?? "system" });
					return true;
				}
				if (method === "PATCH") {
					const input = await body(request);
					if (Object.keys(input).length !== 1 || !["light", "dark", "system"].includes(input.theme as string))
						throw new Error("请求的主题设置无效");
					const result = await writeOnce(request, pathname, input, async () => {
						settingsManager.setTheme(input.theme as string);
						globalEvents.publish("state", { type: "settings_changed" });
						return { theme: input.theme };
					});
					response.setHeader("Set-Cookie", `cst-theme=${result.theme}; Path=/; SameSite=Strict`);
					send(response, 200, result);
					return true;
				}
			}
			if (pathname === "/api/models" && method === "GET") {
				const { modelRuntime, settingsManager } = await pool.getServices();
				const sessionId = new URL(request.url ?? "/", origin).searchParams.get("sessionId");
				const slot = sessionId ? await pool.openSaved(sessionId) : undefined;
				send(response, 200, {
					models: modelRuntime.getModels().map((model) => ({
						id: model.id,
						provider: model.provider,
						name: model.name,
						input: model.input,
						reasoning: model.reasoning,
						contextWindow: model.contextWindow,
						maxTokens: model.maxTokens,
					})),
					enabled: settingsManager.getEnabledModels() ?? [],
					selected: slot?.session.model
						? {
								provider: slot.session.model.provider,
								id: slot.session.model.id,
								thinkingLevel: slot.session.thinkingLevel,
							}
						: null,
				});
				return true;
			}
			if (pathname === "/api/models/select" && method === "POST") {
				const input = await body(request);
				if (
					typeof input.sessionId !== "string" ||
					typeof input.provider !== "string" ||
					typeof input.modelId !== "string"
				)
					throw new Error("请求的模型选择无效");
				const slot = await pool.openSaved(input.sessionId);
				const model = slot.session.modelRuntime.getModel(input.provider, input.modelId);
				if (!model) throw new Error("请求的模型不存在");
				const result = await writeOnce(request, pathname, input, async () => {
					await slot.session.setModel(model);
					return { provider: model.provider, id: model.id };
				});
				send(response, 200, result);
				return true;
			}
			if (pathname === "/api/models/thinking" && method === "POST") {
				const input = await body(request);
				if (typeof input.sessionId !== "string" || typeof input.level !== "string")
					throw new Error("请求的思考强度无效");
				const slot = await pool.openSaved(input.sessionId);
				if (!slot.session.getAvailableThinkingLevels().includes(input.level as never))
					throw new Error("请求的思考强度无效");
				const result = await writeOnce(request, pathname, input, async () => {
					slot.session.setThinkingLevel(input.level as typeof slot.session.thinkingLevel);
					return { level: slot.session.thinkingLevel };
				});
				send(response, 200, result);
				return true;
			}
			if (pathname === "/api/models/scoped" && method === "POST") {
				const input = await body(request);
				if (
					!Array.isArray(input.patterns) ||
					input.patterns.length > 100 ||
					!input.patterns.every((pattern) => typeof pattern === "string" && pattern.length <= 200)
				)
					throw new Error("请求的模型范围无效");
				const { settingsManager, modelRuntime } = await pool.getServices();
				const result = await writeOnce(request, pathname, input, async () => {
					const patterns = input.patterns as string[];
					const scope = await resolveModelScopeWithDiagnostics(patterns, modelRuntime);
					if (scope.diagnostics.length) throw new Error("请求的模型范围没有匹配项");
					settingsManager.setEnabledModels(patterns);
					pool.setScopedModels(scope.scopedModels);
					return { enabled: settingsManager.getEnabledModels() ?? [] };
				});
				send(response, 200, result);
				return true;
			}
			if (pathname === "/api/auth" && method === "GET") {
				const { modelRuntime } = await pool.getServices();
				const credentials = await modelRuntime.listCredentials();
				send(response, 200, {
					providers: modelRuntime.getProviders().map((provider) => ({
						id: provider.id,
						name: provider.name,
						type: credentials.find((credential) => credential.providerId === provider.id)?.type ?? null,
						requiresLogin: invalidAuth.has(provider.id),
					})),
				});
				return true;
			}
			const auth = /^\/api\/auth\/([a-zA-Z0-9_-]{1,128})\/(api-key|logout)$/.exec(pathname);
			if (auth && (method === "PUT" || method === "POST")) {
				const [, providerId, action] = auth;
				const { modelRuntime } = await pool.getServices();
				if (!modelRuntime.getProvider(providerId)) throw new Error("请求的模型服务不存在");
				const input = await body(request);
				if (action === "api-key" && method === "PUT") {
					if (
						typeof input.key !== "string" ||
						!input.key.trim() ||
						input.key.length > 8_192 ||
						Object.keys(input).length !== 1
					)
						throw new Error("请求的密钥格式无效");
					const result = await writeOnce(request, pathname, input, async () => {
						let prompts = 0;
						await modelRuntime.login(providerId, "api_key", {
							prompt: async (question) => {
								if (++prompts !== 1 || question.type !== "secret")
									throw new Error("该模型服务需要其他登录信息");
								return input.key as string;
							},
							notify: () => {},
						});
						invalidAuth.delete(providerId);
						globalEvents.publish("state", { type: "auth_changed", providerId });
						return { providerId, type: "api_key" };
					});
					send(response, 200, result);
					return true;
				}
				if (action === "logout" && method === "POST") {
					if (Object.keys(input).length) throw new Error("请求格式无效");
					const result = await writeOnce(request, pathname, input, async () => {
						await modelRuntime.logout(providerId);
						invalidAuth.delete(providerId);
						globalEvents.publish("state", { type: "auth_changed", providerId });
						return { providerId, type: null };
					});
					send(response, 200, result);
					return true;
				}
			}
			if (pathname === "/api/sessions" && method === "GET") {
				send(response, 200, { sessions: await pool.list() });
				return true;
			}
			if (pathname === "/api/sessions" && method === "POST") {
				const key = request.headers["idempotency-key"];
				if (typeof key !== "string" || !/^[\w-]{1,128}$/.test(key)) throw new Error("缺少有效的去重键");
				const input = await body(request);
				if (
					Object.keys(input).some((field) => !["provider", "modelId", "thinkingLevel"].includes(field)) ||
					(input.provider === undefined) !== (input.modelId === undefined) ||
					(input.provider !== undefined &&
						(typeof input.provider !== "string" || typeof input.modelId !== "string")) ||
					(input.thinkingLevel !== undefined && typeof input.thinkingLevel !== "string")
				)
					throw new Error("请求的新会话配置无效");
				if (
					typeof input.provider === "string" &&
					!(await pool.getServices()).modelRuntime.getModel(input.provider, input.modelId as string)
				)
					throw new Error("请求的模型不存在");
				const result = await writeOnce(request, pathname, input, async () => {
					const slot = await pool.createWithKey(key);
					if (typeof input.provider === "string" && typeof input.modelId === "string") {
						const model = slot.session.modelRuntime.getModel(input.provider, input.modelId);
						if (!model) throw new Error("请求的模型不存在");
						await slot.session.setModel(model);
					}
					if (typeof input.thinkingLevel === "string") {
						if (!slot.session.getAvailableThinkingLevels().includes(input.thinkingLevel as never))
							throw new Error("请求的思考强度无效");
						slot.session.setThinkingLevel(input.thinkingLevel as typeof slot.session.thinkingLevel);
					}
					globalEvents.publish("state", { type: "sessions_changed" });
					return { id: slot.id };
				});
				try {
					await pool.openSaved(result.id);
				} catch (error) {
					if (error instanceof Error && /会话不存在/.test(error.message))
						throw new InboxConflict("创建请求对应的会话已删除");
					throw error;
				}
				send(response, 201, result);
				return true;
			}
			const match = /^\/api\/sessions\/([a-zA-Z0-9_-]{1,128})(?:\/(.*))?$/.exec(pathname);
			if (!match) {
				problem(response, 404, "not_found", "接口不存在");
				return true;
			}
			const [, id, suffix = ""] = match;
			if (suffix === "" && method === "GET") {
				const slot = await pool.openSaved(id);
				send(response, 200, {
					id,
					messages: slot.session.messages,
					running: slot.session.isStreaming,
					queue: await (await inbox(id)).snapshot(),
					ui: slot.ui.snapshot(),
				});
				return true;
			}
			if (suffix === "" && method === "DELETE") {
				const result = await writeOnce(request, pathname, null, async () => {
					const slot = await pool.openSaved(id);
					if (slot.session.isStreaming) throw new InboxConflict("会话正在执行");
					const box = await inbox(id);
					if ((await box.snapshot()).items.some((item) => item.status === "delivering"))
						throw new InboxConflict("会话正在执行");
					await box.close();
					await pool.deleteSaved(id);
					inboxes.delete(id);
					streams.get(id)?.close();
					streams.delete(id);
					await rm(join(agentDir, "web-inbox", `${id}.json`), { force: true });
					await rm(join(agentDir, "web-inbox", "attachments", id), { force: true, recursive: true });
					globalEvents.publish("state", { type: "sessions_changed" });
					return { deleted: true };
				});
				send(response, 200, result);
				return true;
			}
			if (suffix === "" && method === "PATCH") {
				const input = await body(request);
				if (
					typeof input.name !== "string" ||
					!input.name.trim() ||
					input.name.length > 160 ||
					Object.keys(input).length !== 1
				)
					throw new Error("请求的会话名称无效");
				const result = await writeOnce(request, pathname, input, async () => {
					const slot = await pool.openSaved(id);
					slot.session.setSessionName((input.name as string).trim());
					globalEvents.publish("state", { type: "sessions_changed" });
					return { id, name: slot.session.sessionManager.getSessionName() };
				});
				send(response, 200, result);
				return true;
			}
			if (suffix === "fork" && method === "POST") {
				const input = await body(request);
				if (typeof input.entryId !== "string" || Object.keys(input).length !== 1)
					throw new Error("请求的分支节点无效");
				const result = await writeOnce(request, pathname, input, async () => {
					const forked = await pool.fork(id, input.entryId as string);
					globalEvents.publish("state", { type: "sessions_changed" });
					return { id: forked.id };
				});
				send(response, 201, result);
				return true;
			}
			if (suffix === "tree" && method === "GET") {
				const slot = await pool.openSaved(id);
				send(response, 200, { tree: slot.session.sessionManager.getTree() });
				return true;
			}
			if (suffix === "tree/navigate" && method === "POST") {
				const input = await body(request);
				const result = await writeOnce(request, pathname, input, async () => {
					const slot = await pool.openSaved(id);
					if (slot.session.isStreaming) throw new InboxConflict("会话正在执行");
					if (typeof input.entryId !== "string" || !slot.session.sessionManager.getEntry(input.entryId))
						throw new Error("请求的分支节点不存在");
					const navigated = await slot.session.navigateTree(input.entryId);
					if (navigated.cancelled) throw new InboxConflict("分支导航被取消");
					return { id, entryId: input.entryId, editorText: navigated.editorText };
				});
				send(response, 200, result);
				return true;
			}
			if (suffix === "export" && method === "GET") {
				const slot = await pool.openSaved(id);
				const content = slot.session.messages
					.map((message) => {
						const body = "content" in message ? message.content : undefined;
						const text =
							typeof body === "string"
								? body
								: Array.isArray(body)
									? body
											.filter((part) => part.type === "text")
											.map((part) => part.text)
											.join("\n")
									: "";
						return `## ${message.role}\n\n${text}`;
					})
					.join("\n\n");
				response.writeHead(200, {
					"Content-Type": "text/markdown; charset=utf-8",
					"Cache-Control": "no-store",
					"Content-Disposition": `attachment; filename="session-${id}.md"`,
				});
				response.end(content);
				return true;
			}
			if (suffix === "compact" && method === "POST") {
				const input = await body(request);
				if (
					input.instructions !== undefined &&
					(typeof input.instructions !== "string" || input.instructions.length > 20_000)
				)
					throw new Error("请求的压缩提示无效");
				const result = await writeOnce(request, pathname, input, async () => {
					const slot = await pool.openSaved(id);
					if (slot.session.isStreaming) throw new InboxConflict("会话正在执行");
					await slot.session.compact(input.instructions as string | undefined);
					return { id, compacted: true };
				});
				send(response, 200, result);
				return true;
			}
			const uiResponse = /^ui\/([a-zA-Z0-9_-]{1,128})\/response$/.exec(suffix);
			if (uiResponse && method === "POST") {
				const input = await body(request);
				if (!Object.hasOwn(input, "value") || Object.keys(input).length !== 1) throw new Error("回答格式无效");
				const result = await writeOnce(request, pathname, input, async () => {
					const slot = await pool.openSaved(id);
					try {
						slot.ui.respond(uiResponse[1], input.value);
					} catch (error) {
						if (error instanceof Error && error.message === "提问已失效") throw new InboxConflict(error.message);
						throw error;
					}
					return { answered: true };
				});
				send(response, 200, result);
				return true;
			}
			if (suffix === "events" && method === "GET") {
				await inbox(id);
				streams.get(id)?.serve(request, response);
				return true;
			}
			if (suffix === "open" && method === "POST") {
				const result = await writeOnce(request, pathname, null, async () => {
					const slot = await pool.openSaved(id);
					return { id: slot.id, running: slot.session.isStreaming };
				});
				send(response, 200, result);
				return true;
			}
			if (suffix === "close" && method === "POST") {
				// SSE connections unregister on socket close. A page leaving never disposes a running writer.
				const result = await writeOnce(request, pathname, null, async () => ({ closed: true }));
				send(response, 200, result);
				return true;
			}
			if (suffix === "messages" && method === "POST") {
				const input = await body(request, 17 * 1024 * 1024);
				if (Object.keys(input).some((key) => !["id", "text", "delivery", "images", "skill"].includes(key)))
					throw new Error("消息字段不受支持");
				if (
					typeof input.id !== "string" ||
					typeof input.text !== "string" ||
					(input.delivery !== "queue" && input.delivery !== "steer") ||
					(input.skill !== undefined && typeof input.skill !== "string")
				)
					throw new Error("消息格式无效");
				const item = await serialize(id, async () => {
					const box = await inbox(id);
					const refs = await images.accept(id, input.images ?? []);
					return box.accept(
						input.id as string,
						input.text as string,
						input.delivery as Delivery,
						refs,
						input.skill as string | undefined,
					);
				});
				send(response, 202, { item });
				return true;
			}
			const attachment = /^attachments\/([0-9a-f]{64})$/.exec(suffix);
			if (attachment && method === "GET") {
				const box = await inbox(id);
				const ref = (await box.snapshot()).items
					.flatMap((item) => item.images ?? [])
					.find((image) => image.id === attachment[1]);
				if (!ref) {
					problem(response, 404, "not_found", "图片不存在");
					return true;
				}
				const data = await images.read(id, ref.id);
				response.writeHead(200, {
					"Content-Type": ref.mimeType,
					"Content-Length": data.length,
					"Cache-Control": "no-store",
					"X-Content-Type-Options": "nosniff",
				});
				response.end(data);
				return true;
			}
			if (suffix === "abort" && method === "POST") {
				const result = await writeOnce(request, pathname, null, async () => {
					const box = await inbox(id);
					await box.pause();
					await pool.get(id)?.session.abort();
					return { paused: true };
				});
				send(response, 200, result);
				return true;
			}
			const itemMatch = /^queue\/([\w-]{1,128})(?:\/(steer))?$/.exec(suffix);
			if (itemMatch && ["PATCH", "DELETE", "POST"].includes(method)) {
				const input = await body(request);
				if (!Number.isSafeInteger(input.version)) throw new Error("缺少队列版本");
				const box = await inbox(id);
				const edit =
					method === "DELETE"
						? { remove: true }
						: itemMatch[2] === "steer" && method === "POST"
							? { delivery: "steer" as const }
							: method === "PATCH" && typeof input.text === "string"
								? { text: input.text }
								: undefined;
				if (!edit) throw new Error("操作无效");
				const result = await writeOnce(request, pathname, input, async () => ({
					queue: await box.change(itemMatch[1], input.version as number, edit),
				}));
				send(response, 200, result);
				return true;
			}
			if (suffix === "queue/order" && method === "PUT") {
				const input = await body(request);
				if (
					!Number.isSafeInteger(input.version) ||
					!Array.isArray(input.ids) ||
					!input.ids.every((id) => typeof id === "string")
				) {
					throw new Error("队列排序格式无效");
				}
				const result = await writeOnce(request, pathname, input, async () => ({
					queue: await (await inbox(id)).reorder(input.ids as string[], input.version as number),
				}));
				send(response, 200, result);
				return true;
			}
			problem(response, 404, "not_found", "接口不存在");
		} catch (error) {
			const diskCode = (error as NodeJS.ErrnoException).code;
			if (diskCode === "ENOSPC") problem(response, 507, "storage_full", "工具包存储空间不足，输入未被接受");
			else if (["EACCES", "EPERM", "EROFS", "ENOTDIR", "EEXIST"].includes(diskCode ?? ""))
				problem(response, 500, "storage_unavailable", "工具包目录无法写入，输入未被接受");
			else if (error instanceof Error && /Nothing to compact|Already compacted/.test(error.message))
				problem(response, 409, "nothing_to_compact", "当前会话无需压缩");
			else if (error instanceof InboxConflict) problem(response, 409, "conflict", error.message);
			else if (
				error instanceof SyntaxError ||
				(error instanceof Error && /^(请求|消息|队列|缺少|操作|提交|图片|回答|选项)/.test(error.message))
			) {
				problem(response, 400, "invalid_request", error.message);
			} else if (error instanceof Error && /会话已删除/.test(error.message))
				problem(response, 409, "conflict", "创建请求对应的会话已删除");
			else if (error instanceof Error && /会话不存在/.test(error.message))
				problem(response, 404, "not_found", "会话不存在");
			else problem(response, 500, "server_error", "操作未完成，请稍后重试");
		}
		return true;
	};
	return Object.assign(handle, {
		close: async () => {
			await Promise.all([...inboxes.values()].map((box) => box.close()));
			for (const stream of streams.values()) stream.close();
			globalEvents.close();
		},
	});
}
