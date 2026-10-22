import assert from "node:assert/strict";
import { cp, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { RpcClient } from "@earendil-works/pi-coding-agent";
import { startMockOa } from "../../oauth/test/mock-oa.ts";
import { createWebApi } from "../server/api.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { testRoot } from "./support.ts";

test("CSTOA APIKEY与扫码共用身份、积分和模型选择；自动目录恢复、余额扣减刷新与退出", async () => {
	const home = await mkdtemp(join(await testRoot(), "cstoa-credentials-"));
	const previousHome = process.env.PI_CODING_AGENT_DIR;
	const previousHost = process.env.CSTOA_OA_HOST;
	const options = { profile: { balance: 12000 }, keyModels: ["key-model"] };
	const mock = await startMockOa(options);
	process.env.PI_CODING_AGENT_DIR = home;
	process.env.CSTOA_OA_HOST = mock.host;
	await writeFile(join(home, "settings.json"), "{}");
	await cp(fileURLToPath(new URL("../../oauth/", import.meta.url)), join(home, "extensions", "oauth"), {
		recursive: true,
	});
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	let api: ReturnType<typeof createWebApi>;
	const server = createServer((request, response) => {
		void api(request, response, new URL(request.url ?? "/", "http://localhost").pathname);
	});
	await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
	const address = server.address();
	if (!address || typeof address === "string") throw new Error("missing address");
	api = createWebApi(pool, home, address.port);
	const origin = `http://127.0.0.1:${address.port}`;
	const get = async (path: string) => (await fetch(`${origin}${path}`)).json();
	const post = (path: string, body: unknown) =>
		fetch(`${origin}${path}`, {
			method: "POST",
			headers: {
				Origin: origin,
				"X-CST-Web-Request": "1",
				"Content-Type": "application/json",
				"Idempotency-Key": crypto.randomUUID(),
			},
			body: JSON.stringify(body),
		});
	try {
		const status = await get("/api/auth");
		const provider = status.providers.find((item: { id: string }) => item.id === "cstoa");
		assert.equal(provider.supportsApiKey, true);
		assert.equal(provider.supportsOAuth, true);
		assert.equal(
			(await get("/api/models")).models.some((m: { provider: string }) => m.provider === "cstoa"),
			false,
		);
		const start = await post("/api/auth/cstoa/api-key/start", { key: "sk-test-key" });
		assert.equal(start.status, 202);
		for (let attempt = 0; attempt < 100; attempt++) {
			if ((await get("/api/auth/cstoa/api-key/status")).state !== "pending") break;
			await new Promise((resolve) => setTimeout(resolve, 20));
		}
		assert.equal((await get("/api/auth/cstoa/api-key/status")).state, "succeeded");
		assert.deepEqual(
			(await get("/api/models")).models
				.filter((m: { provider: string }) => m.provider === "cstoa")
				.map((m: { id: string }) => m.id),
			["key-model"],
		);
		const account = await get("/api/account");
		assert.equal(account.signedIn, true);
		assert.equal(account.profile.studentId, "20230001");
		assert.equal((await get("/api/quota?provider=cstoa")).balance, 12000);
		const slot = await pool.create();
		const { modelRuntime } = await pool.getServices();
		const model = modelRuntime.getModel("cstoa", "key-model");
		assert.ok(model);
		await slot.session.setModel(model);
		await get(`/api/sessions/${slot.id}`);
		options.profile.balance = 11000;
		await slot.session.prompt("测试积分刷新", { source: "rpc" });
		assert.equal(slot.session.messages.at(-1)?.role, "assistant");
		assert.ok(
			mock.requests.some((r) => r.path === "/v1/chat/completions"),
			JSON.stringify(slot.session.messages.at(-1)),
		);
		assert.equal((await get("/api/quota?provider=cstoa")).balance, 11000);
		const stored = await readFile(join(home, "models-store.json"), "utf8");
		assert.equal(stored.includes("sk-test-key"), false);
		const pi = new RpcClient({
			cliPath: fileURLToPath(
				new URL("../../../../node_modules/@earendil-works/pi-coding-agent/dist/cli.js", import.meta.url),
			),
			cwd: home,
			env: { PI_CODING_AGENT_DIR: home, PI_OFFLINE: "1", CSTOA_OA_HOST: mock.host },
			args: ["--no-session", "--no-context-files"],
		});
		try {
			await pi.start();
			const available = await pi.getAvailableModels();
			assert.ok(
				available.some((model) => model.provider === "cstoa" && model.id === "key-model"),
				`独立 Pi 进程恢复凭据模型: ${JSON.stringify(available.map((model) => `${model.provider}/${model.id}`))}`,
			);
		} finally {
			await pi.stop();
		}
		const other = new WebSessionPool({
			cwd: home,
			agentDir: home,
			sessionDir: join(home, "sessions"),
			withLoader: (load) => load(),
		});
		try {
			assert.deepEqual(
				(await other.getServices()).modelRuntime.getModels("cstoa").map((m) => m.id),
				["key-model"],
			);
		} finally {
			await other.close();
		}
		await assert.rejects(
			modelRuntime.login("cstoa", "api_key", {
				prompt: async () => "invalid-key",
				notify: () => {},
			}),
			/无效/,
		);
		assert.equal((await get("/api/account")).signedIn, true, "登录失败保留已有凭据");
		await modelRuntime.login("cstoa", "oauth", {
			prompt: async () => {
				throw new Error("扫码不应请求密钥");
			},
			notify: () => {},
		});
		const oauthModels = (await get("/api/models")).models.filter((m: { provider: string }) => m.provider === "cstoa");
		assert.equal(
			oauthModels.some((m: { id: string }) => m.id === "key-model"),
			false,
		);
		const oauthModel = modelRuntime.getModel("cstoa", "deepseek-v4.1-flash");
		assert.ok(oauthModel);
		const answer = await modelRuntime.completeSimple(oauthModel, {
			messages: [{ role: "user", content: "测试扫码模型", timestamp: Date.now() }],
		});
		assert.equal(answer.stopReason, "stop");
		assert.ok(mock.requests.some((r) => r.path === "/api/agent/llm/v1/chat/completions"));
		assert.equal((await get("/api/account")).profile.studentId, "20230001");
		await post("/api/auth/cstoa/logout", {});
		assert.equal((await get("/api/account")).signedIn, false);
		assert.equal(
			(await get("/api/models")).models.some((m: { provider: string }) => m.provider === "cstoa"),
			false,
		);
		assert.equal((await get("/api/auth")).providers.find((p: { id: string }) => p.id === "cstoa").type, null);
	} finally {
		await api.close();
		await pool.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await mock.close();
		await rm(home, { recursive: true, force: true });
		if (previousHome === undefined) delete process.env.PI_CODING_AGENT_DIR;
		else process.env.PI_CODING_AGENT_DIR = previousHome;
		if (previousHost === undefined) delete process.env.CSTOA_OA_HOST;
		else process.env.CSTOA_OA_HOST = previousHost;
	}
});
