import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import {
	createAgentSessionFromServices,
	createAgentSessionServices,
	SessionManager,
} from "@earendil-works/pi-coding-agent";
import { registerRuntimeSession } from "../../runtime/adapter.ts";
import { ownSession, SessionOwnerConflict } from "../../runtime/owner.ts";
import { createWebApi } from "../server/api.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { testRoot } from "./support.ts";

test("默认模型缺少密钥时保持身份，拒绝输入；手动选择和默认配置分离", async (t) => {
	const previous = process.env.DEEPSEEK_API_KEY;
	delete process.env.DEEPSEEK_API_KEY;
	t.after(() => {
		if (previous === undefined) delete process.env.DEEPSEEK_API_KEY;
		else process.env.DEEPSEEK_API_KEY = previous;
	});
	const home = await mkdtemp(join(await testRoot(), "manual-model-"));
	await mkdir(join(home, "extensions", "local-check"), { recursive: true });
	await writeFile(
		join(home, "extensions", "local-check", "index.ts"),
		`export default function(pi) { pi.registerCommand("local-check", { handler: async () => { pi.appendEntry("local-check", {}); } }); }`,
	);
	const model = (id: string) => ({
		id,
		name: id,
		reasoning: false,
		input: ["text"],
		contextWindow: 10000,
		maxTokens: 100,
	});
	await writeFile(
		join(home, "settings.json"),
		JSON.stringify({ defaultProvider: "deepseek", defaultModel: "deepseek-flash", defaultTools: ["read"] }),
	);
	await writeFile(
		join(home, "models.json"),
		JSON.stringify({
			providers: {
				deepseek: { models: [model("deepseek-flash")] },
				probe: {
					api: "openai-completions",
					baseUrl: "http://127.0.0.1:9/v1",
					apiKey: "test-only",
					models: [model("one"), model("two")],
				},
			},
		}),
	);
	const options = {
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: <T>(load: () => Promise<T>) => load(),
	};
	const pool = new WebSessionPool(options);
	let api: ReturnType<typeof createWebApi>;
	const server = createServer((req, res) => void api(req, res, new URL(req.url ?? "/", "http://localhost").pathname));
	await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
	const address = server.address();
	if (!address || typeof address === "string") throw new Error("missing address");
	api = createWebApi(pool, home, address.port);
	const origin = `http://127.0.0.1:${address.port}`;
	const post = (path: string, value: unknown) =>
		fetch(`${origin}${path}`, {
			method: "POST",
			headers: {
				Origin: origin,
				"Content-Type": "application/json",
				"X-CST-Web-Request": "1",
				"Idempotency-Key": crypto.randomUUID(),
			},
			body: JSON.stringify(value),
		});
	try {
		const payload = await (await fetch(`${origin}/api/models`)).json();
		assert.equal(payload.selected.provider, "deepseek");
		assert.equal(payload.selected.id, "deepseek-flash");
		assert.equal(payload.selected.credentialReady, false);
		assert.ok(payload.models.some((item: { provider: string }) => item.provider === "probe"));
		assert.equal((await post("/api/sessions", {})).status, 409, "不能因其他服务有凭据而改变默认身份");
		const services = await pool.getServices();
		const isolatedServices = () =>
			createAgentSessionServices({
				cwd: home,
				agentDir: home,
				settingsManager: services.settingsManager,
				modelRuntime: services.modelRuntime,
				resourceLoaderOptions: { noContextFiles: true, noSkills: true },
			});
		const { session: blocked } = await createAgentSessionFromServices({
			services: await isolatedServices(),
			sessionManager: SessionManager.inMemory(home),
			tools: [],
		});
		registerRuntimeSession(blocked, home, undefined, pool.catalog);
		const releaseOwner = await ownSession(home, blocked.sessionId);
		const beforeBinding = blocked.sessionManager.getEntries().length;
		try {
			await assert.rejects(blocked.bindExtensions({ mode: "tui" }), SessionOwnerConflict);
			assert.equal(blocked.sessionManager.getEntries().length, beforeBinding, "取得写权前不修改会话记录");
		} finally {
			releaseOwner();
			blocked.dispose();
		}
		const { session: tui } = await createAgentSessionFromServices({
			services: await isolatedServices(),
			sessionManager: SessionManager.inMemory(home),
			tools: [],
		});
		registerRuntimeSession(tui, home, undefined, pool.catalog);
		try {
			await tui.bindExtensions({ mode: "tui" });
			assert.equal(tui.model?.provider, "deepseek", "TUI 不沿用 SDK 自动选择的其他服务");
			assert.equal(tui.model?.id, "deepseek-flash");
			await assert.rejects(tui.prompt("不应发送"), /No API key found for deepseek/);
			await tui.prompt("/local-check");
			assert.ok(
				tui.sessionManager
					.getEntries()
					.some((entry) => entry.type === "custom" && entry.customType === "local-check"),
				"无密钥时仍允许本地扩展命令",
			);
		} finally {
			tui.dispose();
		}
		const slot = await pool.create();
		assert.equal(slot.session.model?.provider, "deepseek");
		assert.equal(slot.session.model?.id, "deepseek-flash");
		const denied = await post(`/api/sessions/${slot.id}/messages`, { id: "unsent", text: "草稿", delivery: "queue" });
		assert.equal(denied.status, 409);
		assert.equal((await denied.json()).error.code, "credentials_required");
		await assert.rejects(readFile(join(home, "web-inbox", `${slot.id}.json`)), { code: "ENOENT" });
		assert.equal((await post("/api/models/select", { provider: "probe", modelId: "one" })).status, 200);
		assert.equal(
			(await pool.getServices()).settingsManager.getDefaultProvider(),
			"deepseek",
			"首页手动选择不改默认配置",
		);
		assert.equal(
			(await post("/api/models/select", { sessionId: slot.id, provider: "probe", modelId: "two" })).status,
			200,
		);
		assert.equal(slot.session.model?.id, "two");
		assert.equal((await post("/api/models/default", { provider: "probe", modelId: "one" })).status, 200);
		assert.equal(slot.session.model?.id, "two", "保存默认模型不影响现有会话");
		const next = await pool.create();
		assert.equal(next.session.model?.id, "one");
		assert.equal(
			(await post("/api/models/default", { provider: "deepseek", modelId: "deepseek-flash" })).status,
			200,
			"允许保存尚未配置凭据的默认模型",
		);
		assert.equal(next.session.model?.id, "one");
		const keyless = await pool.create();
		assert.equal(keyless.session.model?.provider, "deepseek");
		await keyless.session.modelRuntime.setRuntimeApiKey("deepseek", "test-only");
		assert.equal(
			(await (await fetch(`${origin}/api/models?sessionId=${keyless.id}`)).json()).selected.credentialReady,
			true,
		);
		assert.equal(keyless.session.model?.id, "deepseek-flash");
		await keyless.session.modelRuntime.removeRuntimeApiKey("deepseek");
		const reopened = new WebSessionPool(options);
		try {
			const restored = await reopened.create();
			assert.equal(restored.session.model?.provider, "deepseek");
			assert.equal(restored.session.model?.id, "deepseek-flash");
		} finally {
			await reopened.close();
		}
	} finally {
		await api.close();
		await pool.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await rm(home, { recursive: true, force: true });
	}
});
