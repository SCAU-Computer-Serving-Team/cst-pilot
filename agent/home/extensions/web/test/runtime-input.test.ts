import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createAgentSession, DefaultResourceLoader, SessionManager } from "@earendil-works/pi-coding-agent";
import { refreshRuntime, registerRuntimeSession } from "../../runtime/adapter.ts";
import { ModelCatalog } from "../../runtime/model-catalog.ts";
import { testRoot } from "./support.ts";

test("本地扩展命令绕过配置与模型预检，失效范围不阻止模型菜单刷新", async (t) => {
	const home = await mkdtemp(join(await testRoot(), "runtime-input-"));
	const previous = process.env.PI_CODING_AGENT_DIR;
	process.env.PI_CODING_AGENT_DIR = home;
	const settings = {
		defaultProvider: "cstoa",
		defaultModel: "missing",
		enabledModels: ["cstoa/missing", "probe/one"],
	};
	await writeFile(join(home, "settings.json"), JSON.stringify(settings));
	await writeFile(
		join(home, "models.json"),
		JSON.stringify({
			providers: {
				probe: {
					api: "openai-completions",
					baseUrl: "http://127.0.0.1:9/v1",
					apiKey: "test-only",
					models: [{ id: "one", reasoning: false, input: ["text"], contextWindow: 10000, maxTokens: 100 }],
				},
			},
		}),
	);
	const catalog = new ModelCatalog(home, { automatic: false });
	t.mock.method(catalog, "check", () => {});
	let calls = 0;
	const loader = new DefaultResourceLoader({
		cwd: home,
		agentDir: home,
		noExtensions: true,
		noSkills: true,
		noContextFiles: true,
		additionalExtensionPaths: [fileURLToPath(new URL("../../runtime/index.ts", import.meta.url))],
		extensionFactories: [
			(pi) =>
				pi.registerCommand("local-check", {
					handler: async () => {
						calls++;
					},
				}),
		],
	});
	await loader.reload();
	assert.deepEqual(loader.getExtensions().errors, []);
	const { session } = await createAgentSession({
		cwd: home,
		agentDir: home,
		resourceLoader: loader,
		sessionManager: SessionManager.inMemory(home),
		tools: [],
	});
	registerRuntimeSession(session, home, undefined, catalog);
	try {
		await session.bindExtensions({ mode: "tui" });
		assert.equal(session.model?.provider, "cstoa");
		const refresh = t.mock.method(session.modelRuntime, "refresh");
		const available = t.mock.method(session.modelRuntime, "getAvailable");
		const auth = t.mock.method(session.modelRuntime, "checkAuth");
		await writeFile(join(home, "settings.json"), "{");
		await session.prompt("/local-check");
		assert.equal(calls, 1);
		assert.equal(refresh.mock.callCount(), 0, "本地命令不刷新模型运行时");
		assert.equal(available.mock.callCount(), 0, "本地命令不解析可用范围");
		assert.equal(auth.mock.callCount(), 0, "本地命令不检查凭据");
		await writeFile(join(home, "settings.json"), JSON.stringify(settings));
		await refreshRuntime(session);
		assert.deepEqual(
			session.scopedModels.map(({ model }) => `${model.provider}/${model.id}`),
			["probe/one"],
		);
		await writeFile(join(home, "settings.json"), JSON.stringify({ ...settings, enabledModels: ["cstoa/missing"] }));
		await refreshRuntime(session);
		assert.deepEqual(session.scopedModels, [], "完全失效的范围沿用 Pi 空范围语义");
		available.mock.resetCalls();
		refresh.mock.resetCalls();
		await assert.rejects(session.prompt("请求模型"), /No API key found for cstoa/);
		assert.equal(refresh.mock.callCount(), 1, "Pi input 事件重读请求配置");
		assert.equal(available.mock.callCount(), 0, "发送不做额外的模型范围可用性检查");
		assert.equal(calls, 1);
	} finally {
		session.dispose();
		await catalog.close();
		if (previous === undefined) delete process.env.PI_CODING_AGENT_DIR;
		else process.env.PI_CODING_AGENT_DIR = previous;
		await rm(home, { recursive: true, force: true });
	}
});
