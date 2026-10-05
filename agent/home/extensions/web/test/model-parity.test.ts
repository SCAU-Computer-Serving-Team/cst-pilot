import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { createWebApi } from "../server/api.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { testRoot } from "./support.ts";

test("Web 模型清单与 TUI 共用可用口径、scoped 设置和下一次启动的存储", async () => {
	const home = await mkdtemp(join(await testRoot(), "cst-model-parity-"));
	const config = {
		providers: {
			probe: {
				baseUrl: "http://127.0.0.1:1/v1",
				api: "openai-completions",
				apiKey: "test-only",
				models: ["one", "two"].map((id) => ({
					id,
					name: id,
					reasoning: false,
					input: ["text"],
					contextWindow: 1000,
					maxTokens: 100,
				})),
			},
		},
	};
	await writeFile(join(home, "models.json"), JSON.stringify(config));
	await writeFile(
		join(home, "settings.json"),
		JSON.stringify({ defaultProvider: "probe", defaultModel: "one", enabledModels: ["probe/two"] }),
	);
	const options = {
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: <T>(load: () => Promise<T>) => load(),
	};
	const pool = new WebSessionPool(options);
	let api: ReturnType<typeof createWebApi>;
	const server = createServer((request, response) => {
		void api(request, response, new URL(request.url ?? "/", "http://localhost").pathname);
	});
	await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
	const address = server.address();
	if (!address || typeof address === "string") throw new Error("missing address");
	api = createWebApi(pool, home, address.port);
	const origin = `http://127.0.0.1:${address.port}`;
	try {
		const { modelRuntime } = await pool.getServices();
		const tui = await modelRuntime.getAvailable();
		const web = await (await fetch(`${origin}/api/models`)).json();
		assert.deepEqual(
			web.models.map((m: { provider: string; id: string }) => `${m.provider}/${m.id}`).sort(),
			tui.map((m) => `${m.provider}/${m.id}`).sort(),
		);
		assert.deepEqual(web.enabled, ["probe/two"]);
		const select = (input: unknown, key: string) =>
			fetch(`${origin}/api/models/select`, {
				method: "POST",
				headers: {
					Origin: origin,
					"X-CST-Web-Request": "1",
					"Content-Type": "application/json",
					"Idempotency-Key": key,
				},
				body: JSON.stringify(input),
			});
		assert.equal((await select({ provider: "probe", modelId: "two" }, "home-selection")).status, 200);
		assert.equal(
			(await (await fetch(`${origin}/api/models`)).json()).selected.id,
			"two",
			"首页模型选择写入 TUI 共用默认模型",
		);
		const slot = await pool.createWithKey("picker-session");
		assert.equal(
			(await select({ sessionId: slot.id, provider: "probe", modelId: "one" }, "session-selection")).status,
			200,
		);
		assert.equal(
			(await (await fetch(`${origin}/api/models`)).json()).selected.id,
			"one",
			"工作台模型选择也更新默认模型",
		);
		const saved = await fetch(`${origin}/api/models/scoped`, {
			method: "POST",
			headers: {
				Origin: origin,
				"X-CST-Web-Request": "1",
				"Content-Type": "application/json",
				"Idempotency-Key": "scope-change",
			},
			body: JSON.stringify({ patterns: ["probe/one"] }),
		});
		assert.equal(saved.status, 200);
		const next = new WebSessionPool(options);
		try {
			assert.deepEqual((await next.getServices()).settingsManager.getEnabledModels(), ["probe/one"]);
			assert.equal((await next.getServices()).modelRuntime.getModels("probe").length, 2);
		} finally {
			await next.close();
		}
		// 模拟 TUI 保存服务与模型增删改：Web 保持同一进程，再进入时读取新配置。
		const updated = {
			providers: {
				changed: {
					...config.providers.probe,
					models: [{ ...config.providers.probe.models[0]!, id: "three", name: "改名后的模型" }],
				},
			},
		};
		await writeFile(join(home, "models.json"), JSON.stringify(updated));
		await writeFile(
			join(home, "settings.json"),
			JSON.stringify({ defaultProvider: "changed", defaultModel: "three", enabledModels: ["changed/three"] }),
		);
		const reentered = await (await fetch(`${origin}/api/models`)).json();
		assert.deepEqual(reentered.enabled, ["changed/three"]);
		assert.equal(
			reentered.models.some((model: { provider: string }) => model.provider === "probe"),
			false,
		);
		assert.ok(
			reentered.models.some(
				(model: { provider: string; id: string; name: string }) =>
					model.provider === "changed" && model.id === "three" && model.name === "改名后的模型",
			),
		);
		assert.equal(reentered.selected.provider, "changed");
	} finally {
		await api.close();
		await pool.close();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await rm(home, { recursive: true, force: true });
	}
});
