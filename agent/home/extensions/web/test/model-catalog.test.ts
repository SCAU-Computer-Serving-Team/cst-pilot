import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { initTheme, ModelRuntime, ModelSelectorComponent } from "@earendil-works/pi-coding-agent";
import {
	type Terminal,
	TuiMainScreen,
} from "../../../../node_modules/@earendil-works/pi-coding-agent/node_modules/@earendil-works/pi-tui/dist/index.js";
import { ModelCatalog } from "../../runtime/model-catalog.ts";
import { testRoot } from "./support.ts";

async function fixture(automatic = false) {
	const home = await mkdtemp(join(await testRoot(), "catalog-refresh-"));
	await writeFile(join(home, "auth.json"), JSON.stringify({ deepseek: { type: "api_key", key: "test-only-secret" } }));
	let requests = 0;
	let fail = false;
	let release: (() => void) | undefined;
	let waiting: Promise<void> | undefined;
	const model = {
		id: "catalog-probe",
		name: "目录模型",
		api: "openai-completions",
		provider: "deepseek",
		baseUrl: "https://api.deepseek.com",
		reasoning: false,
		input: ["text"],
		contextWindow: 32768,
		maxTokens: 4096,
		cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
	};
	const server = createServer((request, response) => {
		requests++;
		assert.equal(request.headers.authorization, undefined);
		void (async () => {
			await waiting;
			if (fail) response.writeHead(400).end();
			else
				response
					.writeHead(200, {
						"content-type": "application/json",
						"last-modified": "Thu, 01 Jan 2099 00:00:00 GMT",
						etag: '"catalog-probe"',
					})
					.end(JSON.stringify([model]));
		})();
	});
	await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
	const address = server.address();
	if (!address || typeof address === "string") throw new Error("Missing server");
	const createRuntime = () =>
		ModelRuntime.create({
			authPath: join(home, "auth.json"),
			modelsPath: join(home, "models.json"),
			modelsStorePath: join(home, "models-store.json"),
			catalogBaseUrl: `http://127.0.0.1:${address.port}`,
			refreshOnCreate: false,
		});
	const catalog = new ModelCatalog(home, { createRuntime, automatic });
	const runtime = await createRuntime();
	const detach = catalog.attach(runtime);
	return {
		home,
		runtime,
		catalog,
		model,
		requests: () => requests,
		fail: () => {
			fail = true;
		},
		hold: () => {
			waiting = new Promise<void>((resolve) => {
				release = resolve;
			});
		},
		release: () => release?.(),
		close: async () => {
			release?.();
			detach();
			await catalog.close();
			server.closeAllConnections();
			await new Promise<void>((resolve) => server.close(() => resolve()));
			await rm(home, { recursive: true, force: true });
		},
	};
}

test("OFFLINE=1 下原生模型刷新显式联网，缓存重读仍不联网", async () => {
	const previous = process.env.PI_OFFLINE;
	process.env.PI_OFFLINE = "1";
	const f = await fixture();
	try {
		await f.runtime.refresh({ allowNetwork: false });
		assert.equal(f.requests(), 0);
		const result = await f.runtime.refresh({ providers: ["deepseek"] });
		assert.equal(result.errors.size, 0);
		assert.equal(f.runtime.getModel("deepseek", f.model.id)?.contextWindow, 32768);
		assert.equal(process.env.PI_OFFLINE, "1");
		await f.runtime.refresh({ providers: ["deepseek"] });
		assert.equal(f.requests(), 1);
		await f.runtime.refresh({ providers: ["deepseek"], force: true });
		assert.equal(f.requests(), 2);
		assert.equal((await readFile(join(f.home, "models-store.json"), "utf8")).includes("test-only-secret"), false);
	} finally {
		await f.close();
		if (previous === undefined) delete process.env.PI_OFFLINE;
		else process.env.PI_OFFLINE = previous;
	}
});

test("并发刷新合并，交互侧本地重读不会取消后台目录请求", async () => {
	const f = await fixture();
	try {
		f.hold();
		const first = f.catalog.refresh({ providers: ["deepseek"], force: true });
		const second = f.catalog.refresh({ providers: ["deepseek"], force: true });
		await f.runtime.refresh({ allowNetwork: false });
		f.release();
		assert.equal((await first).aborted, false);
		assert.equal((await second).aborted, false);
		assert.equal(f.requests(), 1);
		assert.ok(f.runtime.getModel("deepseek", f.model.id));
	} finally {
		await f.close();
	}
});

test("目录变化才通知，刷新失败保留模型且只暴露失败供应商", async () => {
	const f = await fixture();
	const updates: boolean[] = [];
	const off = f.catalog.subscribe((event) => {
		updates.push(event.changed);
	});
	try {
		await f.catalog.refresh({ providers: ["deepseek"], force: true });
		await f.catalog.refresh({ providers: ["deepseek"], force: true });
		assert.deepEqual(updates, [true]);
		f.fail();
		const failed = await f.catalog.refresh({ providers: ["deepseek"], force: true });
		assert.deepEqual([...failed.errors.keys()], ["deepseek"]);
		assert.ok(f.runtime.getModel("deepseek", f.model.id));
		assert.deepEqual(updates, [true, false]);
	} finally {
		off();
		await f.close();
	}
});

test("取消一个调用者不取消其他调用者，退出登录会取消旧任务", async () => {
	const f = await fixture();
	try {
		f.hold();
		const controller = new AbortController();
		const cancelled = f.catalog.refresh({ providers: ["deepseek"], force: true, signal: controller.signal });
		const surviving = f.catalog.refresh({ providers: ["deepseek"], force: true });
		controller.abort();
		assert.equal((await cancelled).aborted, true);
		f.release();
		assert.equal((await surviving).aborted, false);
		f.hold();
		const stale = f.catalog.refresh({ providers: ["deepseek"], force: true });
		await f.runtime.logout("deepseek");
		f.release();
		assert.equal((await stale).aborted, true);
		assert.equal(
			(await f.runtime.listCredentials()).some((credential) => credential.providerId === "deepseek"),
			false,
		);
	} finally {
		await f.close();
	}
});

test("启动自动刷新；重复页面检查复用缓存", async () => {
	const f = await fixture(true);
	try {
		await f.catalog.refresh();
		assert.equal(f.requests(), 1);
		assert.ok(f.runtime.getModel("deepseek", f.model.id));
		for (let i = 0; i < 10; i++) f.catalog.check();
		await f.catalog.refresh();
		assert.equal(f.requests(), 1);
	} finally {
		await f.close();
	}
});

test("原生 TUI 选择器先显示缓存，刷新后展示新模型且不改 OFFLINE", async () => {
	const previous = process.env.PI_OFFLINE;
	process.env.PI_OFFLINE = "1";
	const f = await fixture();
	initTheme("dark", false);
	const noop = () => {};
	const terminal: Terminal = {
		columns: 100,
		rows: 40,
		kittyProtocolActive: false,
		start: noop,
		stop: noop,
		drainInput: async () => {},
		write: noop,
		moveBy: noop,
		hideCursor: noop,
		showCursor: noop,
		clearLine: noop,
		clearFromCursor: noop,
		clearScreen: noop,
		setTitle: noop,
		setProgress: noop,
	};
	const tui = new TuiMainScreen(terminal);
	let selector: ModelSelectorComponent | undefined;
	try {
		await f.runtime.refresh({ allowNetwork: false });
		f.hold();
		selector = new ModelSelectorComponent(tui, undefined, f.runtime, [], noop, noop, f.model.id);
		assert.match(selector.render(100).join("\n"), /Refreshing model catalogs/);
		const pending = f.catalog.refresh();
		f.release();
		await pending;
		await new Promise<void>((resolve) => setImmediate(resolve));
		assert.match(selector.render(100).join("\n"), /catalog-probe/);
		assert.match(selector.render(100).join("\n"), /Model catalogs refreshed/);
		assert.equal(process.env.PI_OFFLINE, "1");
	} finally {
		selector?.dispose();
		tui.stop();
		await f.close();
		if (previous === undefined) delete process.env.PI_OFFLINE;
		else process.env.PI_OFFLINE = previous;
	}
});
