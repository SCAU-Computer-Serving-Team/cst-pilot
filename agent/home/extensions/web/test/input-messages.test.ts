import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { MutationReceipts } from "../server/session/receipts.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import type { Message } from "../shared/conversation.ts";
import { freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

test("发送回执只读核对、同文消息关联、实时与历史身份一致，并随宿主重启恢复", { timeout: 45_000 }, async () => {
	const root = await mkdtemp(join(await testRoot(), "input-messages-"));
	let calls = 0;
	const model = createServer(async (request, response) => {
		for await (const _ of request) {
			/* 消费请求正文。 */
		}
		calls++;
		await new Promise((resolve) => setTimeout(resolve, 120));
		response.writeHead(200, { "Content-Type": "text/event-stream" });
		response.end(
			`data: ${JSON.stringify({ id: "mock", choices: [{ index: 0, delta: { role: "assistant", content: "已完成" }, finish_reason: null }] })}\n\ndata: ${JSON.stringify({ id: "mock", choices: [{ index: 0, delta: {}, finish_reason: "stop" }] })}\n\ndata: [DONE]\n\n`,
		);
	});
	await new Promise<void>((resolve) => model.listen(0, "127.0.0.1", resolve));
	await writeFile(join(root, "settings.json"), JSON.stringify({ defaultProvider: "probe", defaultModel: "mock" }));
	await writeFile(
		join(root, "models.json"),
		JSON.stringify({
			providers: {
				probe: {
					baseUrl: `http://127.0.0.1:${(model.address() as AddressInfo).port}/v1`,
					api: "openai-completions",
					apiKey: "test-only",
					models: [{ id: "mock", reasoning: false, input: ["text"], contextWindow: 10000, maxTokens: 100 }],
				},
			},
		}),
	);
	const port = await freePort();
	const origin = `http://127.0.0.1:${port}`;
	const createPool = () =>
		new WebSessionPool({
			cwd: root,
			agentDir: root,
			sessionDir: join(root, "sessions"),
			withLoader: (load) => load(),
		});
	let pool = createPool();
	let api = createWebApi(pool, root, port);
	let server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions: [], stage: "test" }),
		api,
	);
	await listenWebServer(server, port);
	const post = (path: string, body: unknown, key: string) =>
		fetch(origin + path, {
			method: "POST",
			headers: {
				Origin: origin,
				"X-CST-Web-Request": "1",
				"Content-Type": "application/json",
				"Idempotency-Key": key,
			},
			body: JSON.stringify(body),
		});
	const get = async (path: string) => {
		const response = await fetch(origin + path);
		assert.equal(response.status, 200);
		return response.json();
	};
	const events: Message[] = [];
	const streamController = new AbortController();
	let reader: Promise<void> | undefined;
	try {
		assert.deepEqual(await get("/api/sessions/creation/not-created"), { status: "missing" });
		const created = await post("/api/sessions", {}, "create-once");
		assert.equal(created.status, 201);
		const { id } = await created.json();
		assert.deepEqual(await get("/api/sessions/creation/create-once"), { status: "done", sessionId: id });
		const path = `/api/sessions/${id}`;
		assert.deepEqual(await get(`${path}/messages/not-sent`), { item: null });
		assert.equal(calls, 0, "核对回执不向模型投递");
		const stream = await fetch(`${origin}${path}/events`, { signal: streamController.signal });
		reader = (async () => {
			if (!stream.body) return;
			let buffer = "";
			for await (const chunk of stream.body) {
				buffer += new TextDecoder().decode(chunk);
				const frames = buffer.split("\n\n");
				buffer = frames.pop() ?? "";
				for (const frame of frames) {
					const data = frame.split("\n").find((line) => line.startsWith("data: "));
					if (!data) continue;
					const event = JSON.parse(data.slice(6));
					if (event.type === "message_end" && event.message?.role === "user") events.push(event.message);
				}
			}
		})().catch(() => {});
		const first = await post(`${path}/messages`, { id: "first", text: "相同内容", delivery: "queue" }, "first");
		assert.equal(first.status, 202);
		const duplicate = await post(`${path}/messages`, { id: "first", text: "相同内容", delivery: "queue" }, "first");
		assert.equal(duplicate.status, 202);
		const second = await post(`${path}/messages`, { id: "second", text: "相同内容", delivery: "steer" }, "second");
		assert.equal(second.status, 202);
		for (let i = 0; i < 100; i++) {
			const receipt = await get(`${path}/messages/second`);
			const snapshot = await get(path);
			if (
				receipt.item.status === "delivered" &&
				snapshot.messages.filter((message: Message) => message.role === "user").length === 2 &&
				!snapshot.running
			)
				break;
			await new Promise((resolve) => setTimeout(resolve, 30));
		}
		assert.equal((await get(`${path}/messages/second`)).item.status, "delivered");
		const detail = await get(path);
		const users: Message[] = detail.messages.filter((message: Message) => message.role === "user");
		assert.deepEqual(
			users.map((message) => message.clientMessageId),
			["first", "second"],
		);
		assert.deepEqual(
			detail.entries
				.filter((entry: { message: Message }) => entry.message.role === "user")
				.map((entry: { message: Message }) => entry.message.clientMessageId),
			["first", "second"],
		);
		for (let i = 0; i < 50 && events.length < 2; i++) await new Promise((resolve) => setTimeout(resolve, 10));
		assert.deepEqual(
			events.map((message) => message.clientMessageId),
			["first", "second"],
		);
		assert.equal(calls, 2, "相同 ID 重试不重复调用模型");
		streamController.abort();
		await reader;
		await api.close();
		await pool.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		pool = createPool();
		api = createWebApi(pool, root, port);
		server = createWebServer(
			fileURLToPath(new URL("../static/", import.meta.url)),
			port,
			() => ({ sessions: [], stage: "test" }),
			api,
		);
		await listenWebServer(server, port);
		const restored = await get(path);
		assert.deepEqual(
			restored.messages
				.filter((message: Message) => message.role === "user")
				.map((message: Message) => message.clientMessageId),
			["first", "second"],
		);
		assert.equal((await get(`${path}/messages/first`)).item.id, "first");
		assert.deepEqual(await get("/api/sessions/creation/create-once"), { status: "done", sessionId: id });
		assert.equal(calls, 2, "重启和恢复查询不重放消息");
	} finally {
		streamController.abort();
		await reader;
		await api.close();
		await pool.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		model.closeAllConnections();
		await new Promise<void>((resolve) => model.close(() => resolve()));
		await rm(root, { recursive: true, force: true });
	}
});

test("未完成的创建回执可查询，重启后显式重试不会重复执行不确定操作", async () => {
	const root = await mkdtemp(join(await testRoot(), "input-creation-"));
	try {
		const receipts = new MutationReceipts(root);
		await assert.rejects(
			receipts.once("uncertain", "signature", async () => {
				throw new Error("中断");
			}),
		);
		assert.equal((await new MutationReceipts(root).lookup("uncertain"))?.status, "started");
		let replayed = false;
		await assert.rejects(
			new MutationReceipts(root).once("uncertain", "signature", async () => {
				replayed = true;
			}),
		);
		assert.equal(replayed, false);
	} finally {
		await rm(root, { recursive: true, force: true });
	}
});
