import type { IncomingMessage, ServerResponse } from "node:http";
import { join } from "node:path";
import { SessionEvents } from "./events.ts";
import { type Delivery, InboxConflict, SessionInbox } from "./inbox.ts";
import type { WebSessionPool } from "./sessions.ts";

function send(response: ServerResponse, status: number, body: unknown): void {
	response.writeHead(status, { "Content-Type": "application/json; charset=utf-8", "Cache-Control": "no-store" });
	response.end(JSON.stringify(body));
}
function problem(response: ServerResponse, status: number, code: string, message: string): void {
	send(response, status, { error: { code, message } });
}
async function body(request: IncomingMessage): Promise<Record<string, unknown>> {
	if (!request.headers["content-type"]?.startsWith("application/json")) throw new Error("请求须为 JSON");
	let size = 0;
	const parts: Buffer[] = [];
	for await (const chunk of request) {
		size += chunk.length;
		if (size > 128 * 1024) throw new Error("请求内容过大");
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
	const origin = `http://127.0.0.1:${port}`;
	async function inbox(id: string): Promise<SessionInbox> {
		const slot = await pool.openSaved(id);
		let instance = inboxes.get(id);
		if (!streams.has(id)) streams.set(id, new SessionEvents(slot.session));
		if (!instance) {
			instance = new SessionInbox(
				join(agentDir, "web-inbox"),
				id,
				{
					isBusy: () => slot.session.isStreaming,
					prompt: (text) => slot.session.prompt(text, { source: "rpc" }),
					steer: (text) => slot.session.steer(text),
				},
				(snapshot) => streams.get(id)?.publish("queue", snapshot),
			);
			inboxes.set(id, instance);
			slot.session.subscribe((event) => {
				if (event.type === "agent_end") setTimeout(() => instance?.wake(), 0);
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
			if (pathname === "/api/sessions" && method === "GET") {
				send(response, 200, { sessions: await pool.list() });
				return true;
			}
			if (pathname === "/api/sessions" && method === "POST") {
				const key = request.headers["idempotency-key"];
				if (typeof key !== "string" || !/^[\w-]{1,128}$/.test(key)) throw new Error("缺少有效的去重键");
				// Creating sessions needs a durable key mapping; refuse rather than create duplicates on retry.
				problem(response, 501, "not_implemented", "新建会话接口尚未接入去重存储");
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
				});
				return true;
			}
			if (suffix === "events" && method === "GET") {
				await inbox(id);
				streams.get(id)?.serve(request, response);
				return true;
			}
			if (suffix === "open" && method === "POST") {
				const slot = await pool.openSaved(id);
				send(response, 200, { id: slot.id, running: slot.session.isStreaming });
				return true;
			}
			if (suffix === "messages" && method === "POST") {
				const input = await body(request);
				if (Object.keys(input).some((key) => !["id", "text", "delivery"].includes(key)))
					throw new Error("消息字段不受支持");
				if (
					typeof input.id !== "string" ||
					typeof input.text !== "string" ||
					(input.delivery !== "queue" && input.delivery !== "steer")
				)
					throw new Error("消息格式无效");
				const item = await (await inbox(id)).accept(input.id, input.text, input.delivery as Delivery);
				send(response, 202, { item });
				return true;
			}
			if (suffix === "abort" && method === "POST") {
				const box = await inbox(id);
				await box.pause();
				await pool.get(id)?.session.abort();
				send(response, 200, { paused: true });
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
				send(response, 200, { queue: await box.change(itemMatch[1], input.version as number, edit) });
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
				send(response, 200, { queue: await (await inbox(id)).reorder(input.ids, input.version as number) });
				return true;
			}
			problem(response, 404, "not_found", "接口不存在");
		} catch (error) {
			if (error instanceof InboxConflict) problem(response, 409, "conflict", error.message);
			else if (
				error instanceof SyntaxError ||
				(error instanceof Error && /^(请求|消息|队列|缺少|操作|提交)/.test(error.message))
			) {
				problem(response, 400, "invalid_request", error.message);
			} else if (error instanceof Error && /会话不存在/.test(error.message))
				problem(response, 404, "not_found", "会话不存在");
			else problem(response, 500, "server_error", "操作未完成，请稍后重试");
		}
		return true;
	};
	return Object.assign(handle, {
		close: async () => {
			await Promise.all([...inboxes.values()].map((box) => box.close()));
			for (const stream of streams.values()) stream.close();
		},
	});
}
