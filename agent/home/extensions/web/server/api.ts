import { createHash } from "node:crypto";
import { appendFileSync } from "node:fs";
import { rm } from "node:fs/promises";
import type { IncomingMessage, ServerResponse } from "node:http";
import { join } from "node:path";
import type { ToolResultMessage } from "@earendil-works/pi-ai";
import { BitLockerExpiredError, bitLockerWebInteractions } from "../../diagnostics/bitlocker-web.ts";
import { announceProcessClose, beginProcessClose, cancelProcessClose, hintTelemetry } from "../../telemetry/shared.ts";
import { CstoaAccount } from "./api/account.ts";
import { createAuthRoutes } from "./api/auth.ts";
import { sendError } from "./api/errors.ts";
import { body, problem, send } from "./api/http.ts";
import { createModelRoutes } from "./api/models.ts";
import { listProjectFiles } from "./api/project-files.ts";
import { getProviderQuota } from "./api/quota.ts";
import { deliveredRunbookFile, openDeliveredRunbook } from "./api/runbook.ts";
import { type ImageRef, ImageStore } from "./session/attachments.ts";
import { browserJson } from "./session/browser-json.ts";
import { compactSession } from "./session/compaction.ts";
import { SessionEvents } from "./session/events.ts";
import { InboxConflict, SessionInbox } from "./session/inbox.ts";
import { MutationReceipts } from "./session/receipts.ts";
import type { WebSessionPool } from "./session/sessions.ts";
import { WebSettings } from "./session/web-settings.ts";

/** 同源校验、写入串行化与会话路由的统一入口。 */
export function createWebApi(
	pool: WebSessionPool,
	agentDir: string,
	port: number,
	shutdown?: () => void,
	options: { openRunbook?: (file: string) => Promise<void> } = {},
) {
	let stopping = false;
	let exitTask: Promise<void> | undefined;
	let shutdownScheduled = false;
	const accessLogPath = join(agentDir, "web-access.log");
	function accessLog(line: string): void {
		try {
			appendFileSync(accessLogPath, `${new Date().toISOString()} ${line}\n`);
		} catch {
			/* 诊断日志失败不影响请求 */
		}
	}
	const webSettings = new WebSettings(agentDir);
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
	const account = new CstoaAccount();
	const authRoutes = createAuthRoutes({
		pool,
		agentDir,
		invalidAuth,
		globalEvents,
		account,
		writeOnce,
		serialize,
	});
	const modelRoutes = createModelRoutes({ pool, webSettings, globalEvents, writeOnce, serialize });
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
			const bitlocker = bitLockerWebInteractions.get(id);
			bitlocker?.onChange(() =>
				stream.publish("session", {
					type: "bitlocker_requests_changed",
					requests: bitlocker.snapshot(),
					revision: bitlocker.revision,
					epoch: bitlocker.epoch,
				}),
			);
			slot.session.subscribe((event) => {
				if (event.type === "agent_end") globalEvents.publish("state", { type: "usage_changed", sessionId: id });
				if (event.type === "agent_end" && slot.session.model?.provider === "cstoa") {
					account.invalidate();
					globalEvents.publish("state", { type: "quota_changed", providerId: "cstoa" });
				}
				if (["message_start", "message_end", "agent_start", "agent_end", "session_tree"].includes(event.type))
					globalEvents.publish("state", { type: "sessions_changed", sessionId: id });
			});
		}
		if (!instance) {
			instance = new SessionInbox(
				join(agentDir, "web-inbox"),
				id,
				{
					isBusy: () => !slot.session.isIdle,
					prompt: async (text, refs, skill) => {
						const selected = slot.session.model;
						const latest = selected && slot.session.modelRuntime.getModel(selected.provider, selected.id);
						if (latest && (latest.baseUrl !== selected.baseUrl || latest.api !== selected.api))
							await slot.session.setModel(latest);
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
							// prompt 已接收用户消息。模型失败由助手消息呈现，不应标为投递失败。
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
		const startedAt = Date.now();
		const logDone = (how: string, status?: number) => {
			accessLog(`${method} ${pathname} -> ${how}${status ? ` ${status}` : ""} ${Date.now() - startedAt}ms`);
		};
		response.on("close", () => logDone("close", response.statusCode));
		response.on("finish", () => logDone("finish", response.statusCode));
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
			if (pathname === "/api/health" && method === "GET") {
				send(response, 200, { connected: true, stopping });
				return true;
			}
			if (pathname === "/api/lifecycle" && method === "GET") {
				const rows = stopping ? [] : await pool.list();
				send(response, 200, {
					stopping,
					sessions: pool
						.snapshot()
						.map((slot) => ({ ...slot, title: rows.find((row) => row.id === slot.id)?.title ?? "会话" })),
				});
				return true;
			}
			if (pathname === "/api/lifecycle/telemetry" && method === "POST") {
				const { reason } = await body(request);
				if (reason !== "pagehide" && reason !== "pageshow" && reason !== "online") {
					problem(response, 400, "invalid_hint", "无效的遥测提示");
					return true;
				}
				if (!stopping) hintTelemetry(agentDir, reason);
				send(response, 202, { accepted: true });
				return true;
			}
			if (pathname === "/api/lifecycle/exit" && method === "POST") {
				const input = await body(request);
				if (input.confirm !== "stop") throw new Error("请求须明确确认停止任务");
				if (!shutdown) {
					problem(response, 503, "exit_unavailable", "当前运行环境不支持退出程序");
					return true;
				}
				if (!exitTask) {
					stopping = true;
					exitTask = (async () => {
						await Promise.all([...inboxes.values()].map((box) => box.pause()));
						const budget = beginProcessClose(agentDir);
						try {
							await pool.close();
						} catch (error) {
							cancelProcessClose(agentDir, budget);
							throw error;
						}
						announceProcessClose(agentDir);
					})().catch((error) => {
						stopping = false;
						exitTask = undefined;
						throw error;
					});
				}
				await exitTask;
				if (!shutdownScheduled) {
					shutdownScheduled = true;
					response.once("finish", () => {
						setTimeout(shutdown, 100);
					});
				}
				send(response, 200, { exited: true });
				return true;
			}
			if (stopping) {
				problem(response, 409, "stopping", "程序正在退出，请勿继续提交操作");
				return true;
			}
			if (pathname === "/api/events" && method === "GET") {
				globalEvents.serve(request, response);
				return true;
			}
			if (pathname === "/api/settings") {
				if (method === "GET") {
					const { theme } = await webSettings.read();
					send(response, 200, { theme });
					return true;
				}
				if (method === "PATCH") {
					const input = await body(request);
					const { theme } = input;
					if (Object.keys(input).length !== 1 || (theme !== "light" && theme !== "dark" && theme !== "system"))
						throw new Error("请求的主题设置无效");
					const result = await writeOnce(request, pathname, input, async () => {
						await webSettings.save(theme);
						globalEvents.publish("state", { type: "settings_changed" });
						return { theme };
					});
					response.setHeader("Set-Cookie", `cst-theme=${result.theme}; Path=/; SameSite=Strict`);
					send(response, 200, result);
					return true;
				}
			}
			if (pathname === "/api/quota" && method === "GET") {
				const params = new URL(request.url ?? "/", origin).searchParams;
				const providerId = params.get("provider") ?? "";
				const { modelRuntime } = await pool.refreshServices();
				if (providerId === "cstoa" && params.get("refresh") === "1") account.invalidate();
				send(
					response,
					200,
					providerId === "cstoa"
						? await account.getQuota(modelRuntime, invalidAuth.has("cstoa"))
						: await getProviderQuota(providerId, modelRuntime),
				);
				return true;
			}
			if (pathname === "/api/files" && method === "GET") {
				const params = new URL(request.url ?? "/", origin).searchParams;
				const query = (params.get("query") ?? "").toLowerCase();
				const sessionId = params.get("sessionId");
				if (sessionId && !/^[a-zA-Z0-9_-]{1,128}$/.test(sessionId)) throw new Error("请求的会话ID无效");
				const slot = sessionId ? await pool.openSaved(sessionId) : undefined;
				const all = listProjectFiles(slot?.session.sessionManager.getCwd() ?? pool.cwd);
				const matches = (query ? all.filter((file) => file.toLowerCase().includes(query)) : all).slice(0, 50);
				send(response, 200, {
					total: all.length,
					files: matches.map((path) => ({
						path,
						dir: path.includes("/") ? path.slice(0, path.lastIndexOf("/")) : "",
					})),
				});
				return true;
			}
			if (await modelRoutes(request, response, pathname, method)) return true;
			if (await authRoutes(request, response, pathname, method)) return true;
			if (pathname === "/api/sessions" && method === "GET") {
				send(response, 200, { sessions: await pool.list() });
				return true;
			}
			if (pathname === "/api/sessions" && method === "POST") {
				const key = request.headers["idempotency-key"];
				if (typeof key !== "string" || !/^[\w-]{1,128}$/.test(key)) throw new Error("缺少有效的去重键");
				const input = await body(request);
				const { provider, modelId, thinkingLevel } = input;
				if (
					Object.keys(input).some((field) => !["provider", "modelId", "thinkingLevel"].includes(field)) ||
					(provider === undefined) !== (modelId === undefined) ||
					(provider !== undefined && (typeof provider !== "string" || typeof modelId !== "string")) ||
					(thinkingLevel !== undefined && typeof thinkingLevel !== "string")
				)
					throw new Error("请求的新会话配置无效");
				if (
					typeof provider === "string" &&
					typeof modelId === "string" &&
					!(await pool.getServices()).modelRuntime.getModel(provider, modelId)
				)
					throw new Error("请求的模型不存在");
				const result = await writeOnce(request, pathname, input, async () => {
					const slot = await pool.createWithKey(key);
					if (typeof provider === "string" && typeof modelId === "string") {
						const model = slot.session.modelRuntime.getModel(provider, modelId);
						if (!model) throw new Error("请求的模型不存在");
						await serialize("model-default", () => slot.session.setModel(model, { persist: true }));
					}
					if (typeof thinkingLevel === "string") {
						const selected = slot.session.getAvailableThinkingLevels().find((level) => level === thinkingLevel);
						if (!selected) throw new Error("请求的思考强度无效");
						slot.session.setThinkingLevel(selected);
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
			if (suffix === "runbook/open" && method === "POST") {
				const input = await body(request, 4096);
				const { entryId, toolCallId } = input;
				if (typeof entryId !== "string" || typeof toolCallId !== "string" || Object.keys(input).length !== 2)
					throw new Error("请求须指定清单交付记录");
				const result = await writeOnce(request, pathname, input, async () => {
					const slot = await pool.openSaved(id);
					const branch = slot.session.sessionManager.getBranch();
					const index = branch.findIndex((entry) => entry.id === entryId);
					const entry = branch[index];
					if (!entry || entry.type !== "message" || entry.message.role !== "assistant")
						throw new Error("请求对应的清单调用不存在");
					const call = entry.message.content.find(
						(part) =>
							part.type === "toolCall" &&
							part.id === toolCallId &&
							["runbook", "runbook_deliver"].includes(part.name),
					);
					if (!call || call.type !== "toolCall") throw new Error("请求对应的清单调用不存在");
					let delivered: ToolResultMessage | undefined;
					for (const candidate of branch.slice(index + 1)) {
						if (candidate.type !== "message") continue;
						if (candidate.message.role === "assistant" || candidate.message.role === "user") break;
						if (
							candidate.message.role === "toolResult" &&
							candidate.message.toolCallId === toolCallId &&
							candidate.message.toolName === call.name
						) {
							delivered = candidate.message;
							break;
						}
					}
					const early = slot.execution.resultFor(entry.message.timestamp, toolCallId);
					if (!delivered && early?.role === "toolResult" && early.toolName === call.name) delivered = early;
					const file = deliveredRunbookFile(delivered);
					if (typeof file !== "string") throw new Error("请求对应的清单尚未交付或已失效");
					await (options.openRunbook ?? openDeliveredRunbook)(file);
					return { launched: true };
				});
				send(response, 200, result);
				return true;
			}
			if (suffix === "bitlocker" && method === "GET") {
				await pool.openSaved(id);
				const channel = bitLockerWebInteractions.get(id);
				send(response, 200, {
					requests: channel?.snapshot() ?? [],
					revision: channel?.revision ?? 0,
					epoch: channel?.epoch,
				});
				return true;
			}
			if (/^bitlocker\/[a-zA-Z0-9-]+$/.test(suffix) && method === "POST") {
				await pool.openSaved(id);
				const input = await body(request, 4096);
				const channel = bitLockerWebInteractions.get(id);
				if (!channel) throw new BitLockerExpiredError("请求已处理或失效");
				// 凭据不进入持久化去重记录；requestId 在内存中只消费一次。
				channel.respond(suffix.slice("bitlocker/".length), input);
				send(response, 200, { answered: true });
				return true;
			}
			if (suffix === "" && method === "GET") {
				const slot = await pool.openSaved(id);
				send(
					response,
					200,
					{
						id,
						bitlocker: {
							requests: bitLockerWebInteractions.get(id)?.snapshot() ?? [],
							revision: bitLockerWebInteractions.get(id)?.revision ?? 0,
							epoch: bitLockerWebInteractions.get(id)?.epoch,
						},
						messages: slot.session.messages.map((message) => slot.execution.decorate(message)),
						entries: slot.session.sessionManager
							.getBranch()
							// 分支总结条目的 type 是 branch_summary，得单独转成前端认识的消息，否则正文里看不到它。
							.filter((entry) => entry.type === "message" || entry.type === "branch_summary")
							.map((entry) =>
								entry.type === "branch_summary"
									? {
											id: entry.id,
											message: {
												role: "branchSummary",
												parentId: entry.parentId,
												summary: entry.summary,
												fromId: entry.fromId,
												timestamp: new Date(entry.timestamp).getTime(),
											},
										}
									: {
											id: entry.id,
											parentId: entry.parentId,
											message: slot.execution.decorate(entry.message),
										},
							),
						running: slot.session.isStreaming,
						contextUsage: slot.session.getSessionStats().contextUsage ?? null,
						queue: await (await inbox(id)).snapshot(),
						ui: slot.ui.snapshot(),
					},
					browserJson,
				);
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
				const { name } = input;
				if (typeof name !== "string" || !name.trim() || name.length > 160 || Object.keys(input).length !== 1)
					throw new Error("请求的会话名称无效");
				const result = await writeOnce(request, pathname, input, async () => {
					const slot = await pool.openSaved(id);
					slot.session.setSessionName(name.trim());
					globalEvents.publish("state", { type: "sessions_changed" });
					return { id, name: slot.session.sessionManager.getSessionName() };
				});
				send(response, 200, result);
				return true;
			}
			if (suffix === "fork" && method === "POST") {
				const input = await body(request);
				const { entryId } = input;
				if (typeof entryId !== "string" || Object.keys(input).length !== 1) throw new Error("请求的分支节点无效");
				const result = await writeOnce(request, pathname, input, async () => {
					const forked = await pool.fork(id, entryId);
					globalEvents.publish("state", { type: "sessions_changed" });
					return { id: forked.id };
				});
				send(response, 201, result);
				return true;
			}
			if (suffix === "tree" && method === "GET") {
				const slot = await pool.openSaved(id);
				send(response, 200, {
					tree: slot.session.sessionManager.getTree(),
					leafId: slot.session.sessionManager.getLeafId(),
				});
				return true;
			}
			if (suffix === "tree/navigate" && method === "POST") {
				const input = await body(request);
				const { customInstructions } = input;
				if (input.summarize !== undefined && typeof input.summarize !== "boolean")
					throw new Error("请求的总结开关无效");
				if (
					customInstructions !== undefined &&
					(typeof customInstructions !== "string" || customInstructions.length > 20_000)
				)
					throw new Error("请求的总结提示无效");
				const result = await writeOnce(request, pathname, input, async () => {
					const slot = await pool.openSaved(id);
					// 对齐 TUI：提交导航时若正在生成，先停队列与当前生成再导航（同 abort 端点的顺序）；
					// 排队消息留在队列面板，不丢。压缩进行中仍由 navigateTree 拒绝。
					if (slot.session.isStreaming) {
						await (await inbox(id)).pause();
						await slot.session.abort();
					}
					if (typeof input.entryId !== "string" || !slot.session.sessionManager.getEntry(input.entryId))
						throw new Error("请求的分支节点不存在");
					const navigated = await slot.session.navigateTree(input.entryId, {
						summarize: input.summarize === true,
						customInstructions,
					});
					// 取消由用户发起（点取消总结触发 /abort），属于正常结果；报 409 会让前端把主动取消当成冲突。
					if (navigated.cancelled) return { id, entryId: input.entryId, cancelled: true };
					return {
						id,
						entryId: input.entryId,
						cancelled: false,
						editorText: navigated.editorText,
						summaryEntryId: navigated.summaryEntry?.id,
					};
				});
				send(response, 200, result);
				return true;
			}
			if (suffix === "tree/abort" && method === "POST") {
				// 不走 writeOnce：总结进行中时导航请求占着会话写锁，排队会让取消永远迟到。
				// 停一颗中止信号本身幂等，不依赖去重；导航请求会以 cancelled 收尾。
				pool.get(id)?.session.abortBranchSummary();
				send(response, 200, { aborted: true });
				return true;
			}
			if (suffix === "compact" && method === "POST") {
				const input = await body(request);
				const { instructions } = input;
				if (instructions !== undefined && (typeof instructions !== "string" || instructions.length > 20_000))
					throw new Error("请求的压缩提示无效");
				const result = await writeOnce(request, pathname, input, async () => {
					const slot = await pool.openSaved(id);
					if (!slot.session.isIdle) throw new InboxConflict("会话正在执行，请等待完成后再压缩");
					await compactSession(slot.session, instructions);
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
				const { id: messageId, text, delivery, skill } = input;
				if (Object.keys(input).some((key) => !["id", "text", "delivery", "images", "skill"].includes(key)))
					throw new Error("消息字段不受支持");
				if (
					typeof messageId !== "string" ||
					typeof text !== "string" ||
					(delivery !== "queue" && delivery !== "steer") ||
					(skill !== undefined && typeof skill !== "string")
				)
					throw new Error("消息格式无效");
				const item = await serialize(id, async () => {
					const box = await inbox(id);
					const refs = await images.accept(id, input.images ?? []);
					return box.accept(messageId, text, delivery, refs, skill);
				});
				globalEvents.publish("state", { type: "sessions_changed", sessionId: id });
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
			const itemMatch = /^queue\/([\w-]{1,128})(?:\/(steer|retry))?$/.exec(suffix);
			if (itemMatch && ["PATCH", "DELETE", "POST"].includes(method)) {
				const input = await body(request);
				const { version } = input;
				if (typeof version !== "number" || !Number.isSafeInteger(version)) throw new Error("缺少队列版本");
				const box = await inbox(id);
				const edit =
					method === "DELETE"
						? { remove: true }
						: itemMatch[2] === "retry" && method === "POST"
							? { retry: true }
							: itemMatch[2] === "steer" && method === "POST"
								? { delivery: "steer" as const }
								: method === "PATCH" && typeof input.text === "string"
									? { text: input.text }
									: undefined;
				if (!edit) throw new Error("操作无效");
				const result = await writeOnce(request, pathname, input, async () => ({
					queue: await box.change(itemMatch[1], version, edit),
				}));
				send(response, 200, result);
				return true;
			}
			if (suffix === "queue/order" && method === "PUT") {
				const input = await body(request);
				const { ids, version } = input;
				if (
					typeof version !== "number" ||
					!Number.isSafeInteger(version) ||
					!Array.isArray(ids) ||
					!ids.every((id): id is string => typeof id === "string")
				) {
					throw new Error("队列排序格式无效");
				}
				const result = await writeOnce(request, pathname, input, async () => ({
					queue: await (await inbox(id)).reorder(ids, version),
				}));
				send(response, 200, result);
				return true;
			}
			problem(response, 404, "not_found", "接口不存在");
		} catch (error) {
			sendError(response, error);
		}
		return true;
	};
	return Object.assign(handle, {
		close: async () => {
			account.invalidate();
			await authRoutes.close();
			await Promise.all([...inboxes.values()].map((box) => box.close()));
			for (const stream of streams.values()) stream.close();
			globalEvents.close();
		},
	});
}
