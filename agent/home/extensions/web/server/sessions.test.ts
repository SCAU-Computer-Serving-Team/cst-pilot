import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { after, test } from "node:test";
import { WebSessionPool } from "./sessions.ts";

const now = new Date();
const tempRoot = `E:/tmp/${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}-${String(now.getDate()).padStart(2, "0")}`;
await mkdir(tempRoot, { recursive: true });
const agentDir = await mkdtemp(join(tempRoot, "cst-web-pool-"));
const sessionDir = join(agentDir, "sessions");
await writeFile(join(agentDir, "settings.json"), JSON.stringify({ defaultTools: ["read", "ls"] }));
const pool = new WebSessionPool({ cwd: agentDir, agentDir, sessionDir, withLoader: (load) => load() });
after(async () => {
	await pool.close();
	await rm(agentDir, { recursive: true, force: true });
});

test("one owner per session ID and independent execution state", async () => {
	const [a, b] = await Promise.all([pool.create(), pool.create()]);
	assert.notEqual(a.id, b.id);
	assert.equal(pool.get(a.id), a);
	assert.equal(pool.get(b.id), b);
	assert.strictEqual(a.session.settingsManager, b.session.settingsManager);
	assert.strictEqual(a.session.modelRuntime, b.session.modelRuntime);
	assert.notStrictEqual(a.session.resourceLoader, b.session.resourceLoader);
	await writeFile(join(agentDir, "settings.json"), JSON.stringify({ theme: "dark", defaultTools: ["read", "ls"] }));
	await a.session.settingsManager.reload();
	assert.equal(b.session.settingsManager.getTheme(), "dark");
	a.session.modelRuntime.registerProvider("shared-probe", {
		baseUrl: "http://127.0.0.1:1/v1",
		api: "openai-completions",
		models: [
			{
				id: "shared-model",
				name: "shared-model",
				reasoning: false,
				input: ["text"],
				contextWindow: 1000,
				maxTokens: 100,
				cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
			},
		],
	});
	assert.ok(b.session.modelRuntime.getModel("shared-probe", "shared-model"));
	assert.deepEqual(
		pool
			.snapshot()
			.map(({ id }) => id)
			.sort(),
		[a.id, b.id].sort(),
	);
	assert.equal(a.session.getActiveToolNames().includes("bash"), false);
	assert.equal(b.session.getActiveToolNames().includes("write"), false);
	assert.strictEqual(await pool.openSaved(a.id), a);
});

test("saved sessions resolve by ID and concurrent opens reuse the writer", async () => {
	const original = await pool.create();
	const manager = original.session.sessionManager;
	manager.appendMessage({ role: "user", content: [{ type: "text", text: "fixture" }], timestamp: Date.now() });
	const file = manager.getSessionFile();
	assert.ok(file);
	// Reopening a live slot must never create a second writer.
	const [one, two] = await Promise.all([pool.openHandoff(original.id, file), pool.openSaved(original.id)]);
	assert.strictEqual(one, original);
	assert.strictEqual(two, original);
	await assert.rejects(pool.openSaved("../../not-a-session"), /不存在/);
});

test("a saved session reopens by ID after its first writer is disposed", async () => {
	const home = await mkdtemp(join(tempRoot, "cst-web-resume-"));
	const options = {
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: <T>(load: () => Promise<T>) => load(),
	};
	try {
		await writeFile(join(home, "settings.json"), JSON.stringify({ defaultTools: ["read", "ls"] }));
		const first = new WebSessionPool(options);
		const original = await first.create();
		original.session.sessionManager.appendMessage({
			role: "user",
			content: [{ type: "text", text: "saved" }],
			timestamp: Date.now(),
		});
		original.session.sessionManager.appendMessage({
			role: "assistant",
			content: [{ type: "text", text: "done" }],
			api: "openai-completions",
			provider: "probe",
			model: "mock",
			stopReason: "stop",
			usage: {
				input: 0,
				output: 0,
				cacheRead: 0,
				cacheWrite: 0,
				totalTokens: 0,
				cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
			},
			timestamp: Date.now(),
		});
		await first.close();
		const second = new WebSessionPool(options);
		try {
			const reopened = await second.openSaved(original.id);
			assert.equal(reopened.id, original.id);
			assert.equal(reopened.session.messages.length, 2);
			assert.strictEqual(await second.openSaved(original.id), reopened);
		} finally {
			await second.close();
		}
	} finally {
		await rm(home, { recursive: true, force: true });
	}
});

test("two SDK sessions run concurrently and abort stays within one session", async () => {
	const home = await mkdtemp(join(tempRoot, "cst-web-parallel-"));
	const server = createServer(async (request, response) => {
		for await (const _chunk of request) {
			/* consume the request */
		}
		await new Promise((resolve) => setTimeout(resolve, 500));
		response.writeHead(200, { "Content-Type": "text/event-stream" });
		response.write(
			`data: ${JSON.stringify({ id: "x", object: "chat.completion.chunk", choices: [{ index: 0, delta: { role: "assistant", content: "OK" }, finish_reason: null }] })}\n\n`,
		);
		response.end(
			`data: ${JSON.stringify({ id: "x", object: "chat.completion.chunk", choices: [{ index: 0, delta: {}, finish_reason: "stop" }] })}\n\ndata: [DONE]\n\n`,
		);
	});
	try {
		await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
		const address = server.address();
		if (!address || typeof address === "string") throw new Error("missing address");
		await writeFile(
			join(home, "settings.json"),
			JSON.stringify({ defaultProvider: "probe", defaultModel: "mock", defaultTools: ["read", "ls"] }),
		);
		await writeFile(
			join(home, "models.json"),
			JSON.stringify({
				providers: {
					probe: {
						baseUrl: `http://127.0.0.1:${address.port}/v1`,
						api: "openai-completions",
						apiKey: "test-only",
						models: [
							{
								id: "mock",
								name: "mock",
								reasoning: false,
								input: ["text"],
								contextWindow: 10000,
								maxTokens: 300,
							},
						],
					},
				},
			}),
		);
		const parallel = new WebSessionPool({
			cwd: home,
			agentDir: home,
			sessionDir: join(home, "sessions"),
			withLoader: (load) => load(),
		});
		try {
			const [a, b] = await Promise.all([parallel.create(), parallel.create()]);
			const runA = a.session.prompt("A", { expandPromptTemplates: false });
			const runB = b.session.prompt("B", { expandPromptTemplates: false });
			await new Promise((resolve) => setTimeout(resolve, 100));
			assert.equal(a.session.isStreaming, true);
			assert.equal(b.session.isStreaming, true);
			await a.session.abort();
			await Promise.all([runA, runB]);
			assert.equal(b.session.getLastAssistantText(), "OK");
			assert.equal(
				b.session.messages.some(
					(message) =>
						message.role === "user" &&
						Array.isArray(message.content) &&
						message.content.some((part) => part.type === "text" && part.text === "A"),
				),
				false,
			);
		} finally {
			await parallel.close();
		}
	} finally {
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await rm(home, { recursive: true, force: true });
	}
});
