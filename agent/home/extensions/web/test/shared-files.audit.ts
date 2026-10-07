import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { RpcClient } from "@earendil-works/pi-coding-agent";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

/** 独立Pi进程与Web共享验收，真实TUI菜单另由Herdr验证。 */
async function fixture(
	run: (context: {
		home: string;
		cwdA: string;
		cwdB: string;
		requests: { tag: string; agents: boolean; append: boolean }[];
		client: (cwd?: string) => Promise<RpcClient>;
		api: (
			url: string,
			method?: string,
			body?: unknown,
		) => Promise<{
			status: number;
			body: {
				id: string;
				theme: string;
				running: boolean;
				messages: { role: string; content: unknown }[];
				error?: { code: string; message: string };
			};
		}>;
		promptWeb: (id: string, text: string) => Promise<{ messages: { role: string; content: unknown }[] }>;
	}) => Promise<void>,
) {
	const root = join(await testRoot(), "shared-backend-audit");
	await mkdir(root, { recursive: true });
	const dir = await mkdtemp(join(root, "case-")),
		home = join(dir, "home"),
		cwdA = join(dir, "A"),
		cwdB = join(dir, "B");
	for (const p of [home, cwdA, cwdB]) await mkdir(p, { recursive: true });
	await writeFile(join(cwdA, "only-A.txt"), "A");
	await writeFile(join(cwdB, "only-B.txt"), "B");
	await writeFile(join(cwdA, "shared-relative.txt"), "SHARED_A_ONLY");
	await writeFile(join(cwdB, "shared-relative.txt"), "SHARED_B_ONLY");
	await writeFile(join(cwdA, "AGENTS.md"), "UNTRUSTED_SHARED_CONTEXT");
	await writeFile(join(home, "AGENTS.md"), "UNTRUSTED_SHARED_CONTEXT");
	await writeFile(join(home, "APPEND_SYSTEM.md"), "REQUIRED_SHARED_APPEND");
	const requests: { tag: string; agents: boolean; append: boolean }[] = [];
	const model = createServer(async (request, response) => {
		let text = "";
		for await (const c of request) text += c;
		const payload = JSON.parse(text);
		const last = payload.messages.filter((m: { role: string }) => m.role === "user").at(-1);
		const tag = typeof last?.content === "string" ? last.content : JSON.stringify(last?.content);
		const system = JSON.stringify(
			payload.messages.filter((m: { role: string }) => m.role === "system" || m.role === "developer"),
		);
		requests.push({
			tag,
			agents: system.includes("UNTRUSTED_SHARED_CONTEXT"),
			append: system.includes("REQUIRED_SHARED_APPEND"),
		});
		const call = tag.includes("READ_RELATIVE") && payload.messages.at(-1)?.role !== "tool";
		const delta = call
			? {
					role: "assistant",
					tool_calls: [
						{
							index: 0,
							id: "shared-read",
							type: "function",
							function: { name: "read", arguments: JSON.stringify({ path: "shared-relative.txt" }) },
						},
					],
				}
			: { role: "assistant", content: "SHARED_REPLY" };
		response.writeHead(200, { "Content-Type": "text/event-stream" });
		response.end(
			`data: ${JSON.stringify({ id: "shared", choices: [{ index: 0, delta, finish_reason: null }] })}\n\ndata: ${JSON.stringify({ id: "shared", choices: [{ index: 0, delta: {}, finish_reason: call ? "tool_calls" : "stop" }] })}\n\ndata: [DONE]\n\n`,
		);
	});
	const modelPort = await freePort();
	await new Promise<void>((r) => model.listen(modelPort, "127.0.0.1", r));
	await writeFile(
		join(home, "settings.json"),
		JSON.stringify({
			defaultProvider: "audit",
			defaultModel: "one",
			enabledModels: ["audit/one", "audit/two"],
			theme: "light",
			defaultProjectTrust: "never",
			defaultTools: ["read"],
		}),
	);
	await writeFile(
		join(home, "models.json"),
		JSON.stringify({
			providers: {
				audit: {
					baseUrl: `http://127.0.0.1:${modelPort}/v1`,
					api: "openai-completions",
					apiKey: "audit-placeholder",
					models: ["one", "two"].map((id) => ({
						id,
						name: id,
						reasoning: false,
						input: ["text"],
						contextWindow: 16000,
						maxTokens: 100,
					})),
				},
			},
		}),
	);
	const pool = new WebSessionPool({
		cwd: cwdA,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	const port = await freePort(),
		origin = `http://127.0.0.1:${port}`,
		routes = createWebApi(pool, home, port);
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		async () => ({ sessions: await pool.list(), stage: "audit" }),
		routes,
	);
	await listenWebServer(server, port);
	const clients: RpcClient[] = [];
	const api = async (url: string, method = "GET", body?: unknown) => {
		const r = await fetch(origin + url, {
			signal: AbortSignal.timeout(10000),
			method,
			headers:
				method === "GET"
					? {}
					: {
							Origin: origin,
							"X-CST-Web-Request": "1",
							"Content-Type": "application/json",
							"Idempotency-Key": crypto.randomUUID(),
						},
			body: body === undefined ? undefined : JSON.stringify(body),
		});
		return { status: r.status, body: await r.json() };
	};
	const client = async (cwd = cwdA) => {
		const cli = new RpcClient({
			cliPath: fileURLToPath(
				new URL("../../../../node_modules/@earendil-works/pi-coding-agent/dist/cli.js", import.meta.url),
			),
			cwd,
			env: { PI_CODING_AGENT_DIR: home, PI_OFFLINE: "1" },
			args: [
				"--no-context-files",
				"--no-skills",
				"--extension",
				fileURLToPath(new URL("../../runtime/index.ts", import.meta.url)),
			],
		});
		clients.push(cli);
		await cli.start();
		return cli;
	};
	const promptWeb = async (id: string, text: string) => {
		const submission = await api(`/api/sessions/${id}/messages`, "POST", {
			id: crypto.randomUUID(),
			text,
			delivery: "queue",
		});
		assert.equal(submission.status, 202);
		const end = Date.now() + 10000;
		while (Date.now() < end) {
			const detail = await api(`/api/sessions/${id}`);
			const user = detail.body.messages.findIndex(
				(m: { role: string; content: unknown }) => m.role === "user" && JSON.stringify(m.content).includes(text),
			);
			if (
				!detail.body.running &&
				user >= 0 &&
				detail.body.messages.slice(user + 1).some((m: { role: string }) => m.role === "assistant")
			)
				return detail.body;
			await new Promise((r) => setTimeout(r, 30));
		}
		throw new Error("Web审计回复未完成");
	};
	try {
		await run({ home, cwdA, cwdB, requests, client, api, promptWeb });
	} finally {
		for (const cli of clients) await cli.stop();
		await routes.close();
		await pool.close();
		server.closeAllConnections();
		await new Promise<void>((r) => server.close(() => r()));
		model.closeAllConnections();
		await new Promise<void>((r) => model.close(() => r()));
		await rm(dir, { recursive: true, force: true });
	}
}

test("共享模型：Web保存默认后新Pi进程实际读取", { timeout: 30000 }, () =>
	fixture(async ({ api, client }) => {
		await api("/api/models");
		assert.equal((await api("/api/models/select", "POST", { provider: "audit", modelId: "two" })).status, 200);
		const cli = await client();
		assert.equal((await cli.getState()).model?.id, "two");
	}),
);

test("共享上下文：CLI禁用AGENTS，Web接管和新会话保持相同边界", { timeout: 30000 }, () =>
	fixture(async ({ requests, client, api, promptWeb }) => {
		const cli = await client();
		await cli.promptAndWait("CLI_CONTEXT", undefined, 10000);
		const id = (await cli.getState()).sessionId;
		await cli.stop();
		await promptWeb(id, "WEB_CONTEXT");
		const fresh = (await api("/api/sessions", "POST", {})).body.id;
		await promptWeb(fresh, "NEW_WEB_CONTEXT");
		assert.ok(
			requests.every((r) => r.append),
			"所有端实际使用APPEND_SYSTEM",
		);
		assert.deepEqual(
			requests.map((r) => ({ agents: r.agents })),
			[{ agents: false }, { agents: false }, { agents: false }],
			"接管与新会话不可重新开启上下文文件",
		);
	}),
);

test("历史cwd：Web在A打开B会话，相对read仍读取B", { timeout: 30000 }, () =>
	fixture(async ({ cwdB, client, api, promptWeb }) => {
		const cli = await client(cwdB);
		await cli.promptAndWait("TUI_READ_RELATIVE", undefined, 10000);
		const id = (await cli.getState()).sessionId;
		await cli.stop();
		const detail = await promptWeb(id, "WEB_READ_RELATIVE");
		const bFiles = await api(`/api/files?sessionId=${id}`);
		assert.ok(JSON.stringify(bFiles.body).includes("only-B.txt"));
		assert.ok(!JSON.stringify(bFiles.body).includes("only-A.txt"));
		const results = detail.messages.filter((m: { role: string }) => m.role === "toolResult");
		const first = results[0];
		assert.ok(first);
		assert.ok(JSON.stringify(first.content).includes("SHARED_B_ONLY"));
		const last = results.at(-1);
		assert.ok(last);
		assert.ok(JSON.stringify(last.content).includes("SHARED_B_ONLY"), "原会话cwd必须用于Web的实际工具路径");
	}),
);

test("单写权：另一Pi进程仍持有会话时Web不可再接受写入", { timeout: 30000 }, () =>
	fixture(async ({ client, api }) => {
		const cli = await client();
		await cli.promptAndWait("LIVE_OWNER", undefined, 10000);
		const id = (await cli.getState()).sessionId;
		const file = (await cli.getState()).sessionFile;
		assert.ok(file);
		const before = await readFile(file, "utf8");
		const response = await api(`/api/sessions/${id}/messages`, "POST", {
			id: crypto.randomUUID(),
			text: "OTHER_WRITER",
			delivery: "queue",
		});
		if (response.status === 202) {
			const end = Date.now() + 10000;
			while (Date.now() < end && (await api(`/api/sessions/${id}`)).body.running)
				await new Promise((r) => setTimeout(r, 30));
		}
		assert.equal(response.status, 409, "同一记录不可同时存在两个独立写入实例");
		assert.equal(await readFile(file, "utf8"), before, "拒绝前不得更改原记录");
		await cli.stop();
		const retry = await api(`/api/sessions/${id}/open`, "POST", {});
		assert.equal(retry.status, 200, "进程停止后释放写权");
	}),
);

test("独立主题：TUI更改主题不影响Web，Web保存不修改Pi", { timeout: 30000 }, () =>
	fixture(async ({ home, api }) => {
		const file = join(home, "settings.json");
		assert.equal((await api("/api/settings")).body.theme, "light");
		const settings = JSON.parse(await readFile(file, "utf8"));
		await writeFile(file, JSON.stringify({ ...settings, theme: "dark/dark", sentinel: "retained" }));
		assert.equal((await api("/api/settings")).body.theme, "light");
		const before = await readFile(file, "utf8");
		assert.equal((await api("/api/settings", "PATCH", { theme: "dark" })).status, 200);
		assert.equal(await readFile(file, "utf8"), before, "Web不能改写TUI配置");
		assert.equal((await api("/api/settings")).body.theme, "dark");
		await writeFile(file, JSON.stringify({ ...settings, theme: "light" }));
		assert.equal((await api("/api/settings")).body.theme, "dark");
		await writeFile(join(home, "web-settings.json"), JSON.stringify({ theme: "system" }));
		assert.equal((await api("/api/settings")).body.theme, "system");
		await writeFile(join(home, "web-settings.json"), '{"theme":"invalid-private-value"');
		const broken = await api("/api/settings");
		assert.equal(broken.status, 503);
		assert.ok(!JSON.stringify(broken.body).includes("invalid-private-value"));
		assert.equal((await api("/api/settings", "PATCH", { theme: "light" })).status, 200);
		assert.equal((await api("/api/settings")).body.theme, "light");
	}),
);

test("损坏共享配置：返回可操作的文件提示，修正后立即恢复", { timeout: 30000 }, () =>
	fixture(async ({ home, api }) => {
		assert.equal((await api("/api/models")).status, 200);
		for (const name of ["models.json", "settings.json"]) {
			const file = join(home, name),
				before = await readFile(file, "utf8");
			try {
				await writeFile(file, "{");
				const failure = await api("/api/models");
				assert.equal(failure.status, 503);
				assert.equal(failure.body.error?.code, "configuration_unavailable");
				assert.ok(failure.body.error?.message.includes(name));
			} finally {
				await writeFile(file, before);
			}
			assert.equal((await api("/api/models")).status, 200, "修正文件后无需重启");
		}
	}),
);

test("共享范围：运行中Pi重新进入循环读取Web保存范围", { timeout: 30000 }, () =>
	fixture(async ({ client, api }) => {
		const cli = await client();
		await cli.getState();
		await api("/api/models");
		await api("/api/models/scoped", "POST", { patterns: ["audit/two"] });
		const models = [];
		for (let i = 0; i < 3; i++) {
			const result = await cli.cycleModel();
			models.push(result?.model.id ?? (await cli.getState()).model?.id);
		}
		assert.deepEqual(models, ["two", "two", "two"], "保存的单模型范围不能循环到范围外模型");
	}),
);
