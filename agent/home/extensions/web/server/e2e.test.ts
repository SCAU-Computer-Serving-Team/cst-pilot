import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { type AddressInfo, createServer as createNetServer } from "node:net";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "./api.ts";
import { createWebServer, listenWebServer } from "./http.ts";
import { WebSessionPool } from "./sessions.ts";

const now = new Date();
const date = `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}-${String(now.getDate()).padStart(2, "0")}`;
await mkdir(join("E:/tmp", date), { recursive: true });
async function availablePort(): Promise<number> {
	const probe = createNetServer();
	await new Promise<void>((resolve) => probe.listen(0, "127.0.0.1", resolve));
	const port = (probe.address() as AddressInfo).port;
	await new Promise<void>((resolve) => probe.close(() => resolve()));
	return port;
}

test("HTTP submits to two Pi sessions, returns immediately, streams results, and does not replay on refresh", async () => {
	const home = await mkdtemp(join("E:/tmp", date, "cst-web-e2e-"));
	let modelCalls = 0;
	const modelRequests: string[] = [];
	const model = createServer(async (request, response) => {
		const chunks: Buffer[] = [];
		for await (const chunk of request) chunks.push(chunk);
		const payload = Buffer.concat(chunks).toString("utf8");
		modelRequests.push(payload);
		modelCalls++;
		if (payload.includes("force-auth-error")) {
			response.writeHead(401, { "Content-Type": "application/json" });
			response.end(JSON.stringify({ error: { message: "Unauthorized" } }));
			return;
		}
		await new Promise((resolve) =>
			setTimeout(resolve, payload.includes("running task") && !payload.includes("interrupt insert") ? 1200 : 180),
		);
		response.writeHead(200, { "Content-Type": "text/event-stream" });
		response.write(
			`data: ${JSON.stringify({ id: "mock", object: "chat.completion.chunk", choices: [{ index: 0, delta: { role: "assistant", content: "OK" }, finish_reason: null }] })}\n\n`,
		);
		response.end(
			`data: ${JSON.stringify({ id: "mock", object: "chat.completion.chunk", choices: [{ index: 0, delta: {}, finish_reason: "stop" }] })}\n\ndata: [DONE]\n\n`,
		);
	});
	await new Promise<void>((resolve) => model.listen(0, "127.0.0.1", resolve));
	const modelPort = (model.address() as AddressInfo).port;
	const port = await availablePort();
	const origin = `http://127.0.0.1:${port}`;
	const skillDir = join(home, "skills", "disk");
	await mkdir(skillDir, { recursive: true });
	await writeFile(
		join(skillDir, "SKILL.md"),
		"---\nname: disk\ndescription: Test disk skill\n---\nSkill-token-check\n",
	);
	const commandDir = join(home, "extensions", "ask");
	await mkdir(commandDir, { recursive: true });
	await writeFile(
		join(commandDir, "index.ts"),
		`export default function(pi) { pi.registerCommand("ask", { description: "Should not run from chat", handler: async () => { throw new Error("unexpected command execution"); } }); }`,
	);
	await writeFile(
		join(home, "settings.json"),
		JSON.stringify({ defaultProvider: "probe", defaultModel: "mock", defaultTools: ["read", "ls"] }),
	);
	await writeFile(
		join(home, "models.json"),
		JSON.stringify({
			providers: {
				probe: {
					baseUrl: `http://127.0.0.1:${modelPort}/v1`,
					api: "openai-completions",
					models: [
						{
							id: "mock",
							name: "mock",
							reasoning: false,
							input: ["text", "image"],
							contextWindow: 10000,
							maxTokens: 300,
						},
					],
				},
			},
		}),
	);
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	const api = createWebApi(pool, home, port);
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions: pool.snapshot(), stage: "test" }),
		api,
	);
	try {
		await listenWebServer(server, port);
		const send = async (path: string, input: unknown, key?: string) =>
			fetch(`${origin}${path}`, {
				method: "POST",
				headers: {
					Origin: origin,
					"X-CST-Web-Request": "1",
					"Content-Type": "application/json",
					"Idempotency-Key": key ?? randomUUID(),
				},
				body: JSON.stringify(input),
			});
		const login = await fetch(`${origin}/api/auth/probe/api-key`, {
			method: "PUT",
			headers: {
				Origin: origin,
				"X-CST-Web-Request": "1",
				"Content-Type": "application/json",
				"Idempotency-Key": randomUUID(),
			},
			body: JSON.stringify({ key: "mock-secret" }),
		});
		assert.equal(login.status, 200);
		const [one, two] = await Promise.all([
			send("/api/sessions", { provider: "probe", modelId: "mock", thinkingLevel: "off" }, "new-one").then(
				(response) => response.json(),
			),
			send("/api/sessions", {}, "new-two").then((response) => response.json()),
		]);
		assert.notEqual(one.id, two.id);
		const models = await fetch(`${origin}/api/models?sessionId=${one.id}`);
		assert.equal(models.status, 200);
		const available = await models.json();
		assert.ok(
			available.models.some(
				(entry: { provider: string; id: string }) => entry.provider === "probe" && entry.id === "mock",
			),
		);
		assert.equal(available.selected.id, "mock");
		assert.equal(available.selected.thinkingLevel, "off");
		const scope = await send("/api/models/scoped", { patterns: ["probe/mock"] });
		assert.equal(scope.status, 200);
		assert.equal(pool.get(one.id)?.session.scopedModels[0]?.model.id, "mock");
		const selected = await send("/api/models/select", { sessionId: one.id, provider: "probe", modelId: "mock" });
		assert.equal(selected.status, 200);
		const thinking = await send("/api/models/thinking", { sessionId: one.id, level: "off" });
		assert.equal(thinking.status, 200);
		const started = Date.now();
		const png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVQIHWP4z8DwHwAFgAI/ScL/nwAAAABJRU5ErkJggg==";
		const [a, b] = await Promise.all([
			send(`/api/sessions/${one.id}/messages`, {
				id: "a",
				text: "first",
				delivery: "queue",
				images: [{ mimeType: "image/png", data: png }],
			}),
			send(`/api/sessions/${two.id}/messages`, { id: "b", text: "second", delivery: "queue" }),
		]);
		assert.equal(a.status, 202);
		assert.equal(b.status, 202);
		assert.ok(Date.now() - started < 180, "acceptance must not wait for the model");
		for (let i = 0; i < 120; i++) {
			const [left, right] = await Promise.all(
				[one, two].map(async ({ id }) => await (await fetch(`${origin}/api/sessions/${id}`)).json()),
			);
			if (
				left.messages.some((message: { role: string }) => message.role === "assistant") &&
				right.messages.some((message: { role: string }) => message.role === "assistant")
			)
				break;
			await new Promise((resolve) => setTimeout(resolve, 50));
		}
		assert.equal(
			modelCalls,
			2,
			JSON.stringify(
				await Promise.all(
					[one, two].map(async ({ id }) => (await (await fetch(`${origin}/api/sessions/${id}`)).json()).queue),
				),
			),
		);
		assert.ok(
			modelRequests.some((text) => text.includes(png)),
			"model request must contain the accepted image",
		);
		const sessionPath = `/api/sessions/${one.id}`;
		const rename = await fetch(`${origin}${sessionPath}`, {
			method: "PATCH",
			headers: {
				Origin: origin,
				"X-CST-Web-Request": "1",
				"Content-Type": "application/json",
				"Idempotency-Key": randomUUID(),
			},
			body: JSON.stringify({ name: "测试会话" }),
		});
		assert.equal(rename.status, 200);
		const tree = await fetch(`${origin}${sessionPath}/tree`);
		assert.equal(tree.status, 200);
		const nodes = (await tree.json()).tree;
		assert.ok(nodes.length > 0);
		const forked = await send(`${sessionPath}/fork`, { entryId: nodes[0].entry.id }, "fork-once");
		assert.equal(forked.status, 201);
		const branch = await forked.json();
		assert.notEqual(branch.id, one.id);
		assert.equal((await fetch(`${origin}/api/sessions/${branch.id}`)).status, 200);
		const exported = await fetch(`${origin}${sessionPath}/export`);
		assert.equal(exported.status, 200);
		assert.ok((await exported.text()).includes("OK"));
		const compacted = await send(`${sessionPath}/compact`, { instructions: "简短概括" });
		assert.equal(compacted.status, 409);
		assert.equal((await compacted.json()).error.code, "nothing_to_compact");
		const again = await send(`/api/sessions/${one.id}/messages`, {
			id: "a",
			text: "first",
			delivery: "queue",
			images: [{ mimeType: "image/png", data: png }],
		});
		assert.equal(again.status, 202);
		await new Promise((resolve) => setTimeout(resolve, 150));
		assert.equal(modelCalls, 2);
		const interrupted = await send(`${sessionPath}/messages`, {
			id: "interrupt-me",
			text: "long task",
			delivery: "queue",
		});
		assert.equal(interrupted.status, 202);
		const queued = await send(`${sessionPath}/messages`, {
			id: "after-interrupt",
			text: "queued task",
			delivery: "queue",
		});
		assert.equal(queued.status, 202);
		const stop = await send(`${sessionPath}/abort`, {});
		assert.equal(stop.status, 200);
		const paused = await (await fetch(`${origin}${sessionPath}`)).json();
		assert.equal(paused.queue.paused, true);
		assert.equal(paused.queue.items.find((item: { id: string }) => item.id === "after-interrupt").status, "pending");
		const mutateQueue = (method: string, suffix: string, input: unknown, key = randomUUID()) =>
			fetch(`${origin}${sessionPath}/${suffix}`, {
				method,
				headers: {
					Origin: origin,
					"X-CST-Web-Request": "1",
					"Content-Type": "application/json",
					"Idempotency-Key": key,
				},
				body: JSON.stringify(input),
			});
		const ordered = await mutateQueue("PUT", "queue/order", {
			version: paused.queue.version,
			ids: ["after-interrupt"],
		});
		assert.equal(ordered.status, 200);
		const currentVersion = (await ordered.json()).queue.version;
		const editKey = randomUUID();
		const editInput = { version: currentVersion, text: "edited queued task" };
		const edited = await mutateQueue("PATCH", "queue/after-interrupt", editInput, editKey);
		assert.equal(edited.status, 200);
		assert.equal((await mutateQueue("PATCH", "queue/after-interrupt", editInput, editKey)).status, 200);
		assert.equal((await mutateQueue("PATCH", "queue/after-interrupt", editInput)).status, 409);
		const beforeResume = modelCalls;
		await new Promise((resolve) => setTimeout(resolve, 250));
		assert.equal(modelCalls, beforeResume, "stopped queue must not run in the background");
		await send(`${sessionPath}/messages`, { id: "resume-queue", text: "resume", delivery: "queue" });
		for (let i = 0; i < 120; i++) {
			const detail = await (await fetch(`${origin}${sessionPath}`)).json();
			if (detail.queue.items.find((item: { id: string }) => item.id === "resume-queue")?.status === "delivered")
				break;
			await new Promise((resolve) => setTimeout(resolve, 50));
		}
		assert.ok(modelCalls >= beforeResume + 2);
		assert.ok(modelRequests.some((text) => text.includes("edited queued task")));
		const live = await fetch(`${origin}${sessionPath}/events`);
		const liveReader = live.body?.getReader();
		assert.ok(liveReader);
		await liveReader.read(); // connected marker
		const renameAgain = async (name: string) =>
			fetch(`${origin}${sessionPath}`, {
				method: "PATCH",
				headers: {
					Origin: origin,
					"X-CST-Web-Request": "1",
					"Content-Type": "application/json",
					"Idempotency-Key": randomUUID(),
				},
				body: JSON.stringify({ name }),
			});
		assert.equal((await renameAgain("第一次更名")).status, 200);
		let firstFrame = "";
		for (let i = 0; i < 10 && !firstFrame.includes("id: "); i++)
			firstFrame += new TextDecoder().decode((await liveReader.read()).value);
		const lastId = /id: ([^\n]+)/.exec(firstFrame)?.[1];
		assert.ok(lastId);
		await liveReader.cancel();
		assert.equal((await renameAgain("第二次更名")).status, 200);
		const continued = await fetch(`${origin}${sessionPath}/events`, { headers: { "Last-Event-ID": lastId } });
		const continuedReader = continued.body?.getReader();
		assert.ok(continuedReader);
		const replay = new TextDecoder().decode((await continuedReader.read()).value);
		assert.match(replay, /session_info_changed/);
		await continuedReader.cancel();
		const events = await fetch(`${origin}/api/sessions/${one.id}/events`, {
			headers: { "Last-Event-ID": "previous-process:2" },
		});
		assert.match(new TextDecoder().decode((await events.body?.getReader().read())?.value), /event: reset/);
		await events.body?.cancel().catch(() => undefined);
		const navigated = await send(`${sessionPath}/tree/navigate`, { entryId: nodes[0].entry.id });
		assert.equal(navigated.status, 200);
		assert.equal(
			(await send(`${sessionPath}/messages`, { id: "literal-command", text: "/ask", delivery: "queue" })).status,
			202,
		);
		assert.equal(
			(
				await send(`${sessionPath}/messages`, {
					id: "explicit-skill",
					text: "diagnose",
					delivery: "queue",
					skill: "disk",
				})
			).status,
			202,
		);
		for (let i = 0; i < 120; i++) {
			const state = await (await fetch(`${origin}${sessionPath}`)).json();
			if (state.queue.items.find((item: { id: string }) => item.id === "explicit-skill")?.status === "delivered")
				break;
			await new Promise((resolve) => setTimeout(resolve, 50));
		}
		assert.ok(
			modelRequests.some((text) => text.includes("/ask")),
			"ordinary slash text must reach the model",
		);
		assert.ok(
			modelRequests.some((text) => text.includes("Skill-token-check")),
			"only explicit skill invocation expands the skill",
		);
		await send(`${sessionPath}/messages`, { id: "steer-anchor", text: "running task", delivery: "queue" });
		for (let i = 0; i < 40; i++) {
			if ((await (await fetch(`${origin}${sessionPath}`)).json()).running) break;
			await new Promise((resolve) => setTimeout(resolve, 10));
		}
		assert.equal((await send(`${sessionPath}/close`, {})).status, 200);
		await send(`${sessionPath}/messages`, { id: "steer-item", text: "interrupt insert", delivery: "queue" });
		const queue = (await (await fetch(`${origin}${sessionPath}`)).json()).queue;
		const steering = await send(`${sessionPath}/queue/steer-item/steer`, { version: queue.version });
		assert.equal(steering.status, 200);
		for (let i = 0; i < 120; i++) {
			if (modelRequests.some((text) => text.includes("interrupt insert"))) break;
			await new Promise((resolve) => setTimeout(resolve, 50));
		}
		assert.ok(modelRequests.some((text) => text.includes("interrupt insert")));
		const authCalls = modelCalls;
		assert.equal(
			(await send(`${sessionPath}/messages`, { id: "expired-key", text: "force-auth-error", delivery: "queue" }))
				.status,
			202,
		);
		for (let i = 0; i < 120; i++) {
			const state = await (await fetch(`${origin}${sessionPath}`)).json();
			if (state.queue.items.find((item: { id: string }) => item.id === "expired-key")?.status === "failed") break;
			await new Promise((resolve) => setTimeout(resolve, 50));
		}
		assert.equal(modelCalls, authCalls + 1);
		const auth = await (await fetch(`${origin}/api/auth`)).json();
		assert.equal(auth.providers.find((provider: { id: string }) => provider.id === "probe").requiresLogin, true);
	} finally {
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await pool.close();
		await api.close();
		await new Promise<void>((resolve) => model.close(() => resolve()));
		await rm(home, { recursive: true, force: true });
	}
});
