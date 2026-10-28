import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { ModelRuntime } from "@earendil-works/pi-coding-agent";
import { ModelCatalog } from "../../runtime/model-catalog.ts";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

test("模型目录手动更新、后台 SSE 同步与浅深窄屏按钮", { timeout: 60_000 }, async () => {
	const root = join(await testRoot(), "model-catalog-ui");
	await mkdir(root, { recursive: true });
	const home = await mkdtemp(join(root, "home-"));
	await writeFile(join(home, "auth.json"), JSON.stringify({ deepseek: { type: "api_key", key: "local-test-only" } }));
	await writeFile(
		join(home, "settings.json"),
		JSON.stringify({ defaultProvider: "deepseek", defaultModel: "catalog-one" }),
	);
	let requests = 0;
	let ids = ["catalog-one"];
	let fail = false;
	let hold: Promise<void> | undefined;
	let release: (() => void) | undefined;
	const upstream = createServer((request, response) => {
		assert.equal(request.headers.authorization, undefined);
		requests++;
		void (async () => {
			await hold;
			if (fail) {
				response.writeHead(400).end();
				return;
			}
			response.writeHead(200, {
				"content-type": "application/json",
				"last-modified": "Thu, 01 Jan 2099 00:00:00 GMT",
			});
			response.end(
				JSON.stringify(
					ids.map((id) => ({
						id,
						name: id,
						provider: "deepseek",
						api: "openai-completions",
						baseUrl: "https://api.deepseek.com",
						reasoning: false,
						input: ["text"],
						contextWindow: 32768,
						maxTokens: 4096,
						cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
					})),
				),
			);
		})();
	});
	const upstreamPort = await freePort();
	await new Promise<void>((resolve) => upstream.listen(upstreamPort, "127.0.0.1", resolve));
	const catalog = new ModelCatalog(home, {
		automatic: false,
		createRuntime: () =>
			ModelRuntime.create({
				authPath: join(home, "auth.json"),
				modelsPath: join(home, "models.json"),
				modelsStorePath: join(home, "models-store.json"),
				catalogBaseUrl: `http://127.0.0.1:${upstreamPort}`,
				refreshOnCreate: false,
			}),
	});
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		catalog,
		withLoader: (load) => load(),
	});
	const port = await freePort();
	const origin = `http://127.0.0.1:${port}`;
	const api = createWebApi(pool, home, port);
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions: [], stage: "catalog-test" }),
		api,
	);
	let browser: BrowserProbe | undefined;
	try {
		await pool.getServices();
		await catalog.refresh({ force: true });
		await listenWebServer(server, port);
		browser = await BrowserProbe.launch(root);
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 1440,
			height: 1100,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await browser.navigate(`${origin}/settings/add`);
		await browser.until(
			"document.querySelector('.settings-catalog-refresh')?.textContent.includes('更新模型')",
			"更新模型按钮",
		);
		await browser.screenshot(join(root, "refresh-light.png"));
		const geometry = await browser.evaluate<{ height: number; right: number; titleRight: number }>(
			"(()=>{const b=document.querySelector('.settings-catalog-refresh').getBoundingClientRect(),h=document.querySelector('.settings-page-heading h2').getBoundingClientRect();return {height:b.height,right:b.right,titleRight:h.right}})()",
		);
		assert.equal(geometry.height, 32);
		assert.ok(geometry.right > geometry.titleRight);
		hold = new Promise<void>((resolve) => {
			release = resolve;
		});
		const before = requests;
		await browser.click(".settings-catalog-refresh");
		await browser.until(
			"document.querySelector('.settings-catalog-refresh')?.disabled && document.querySelector('.settings-catalog-refresh')?.getAttribute('aria-busy')==='true'",
			"刷新中禁用重复点击",
		);
		release?.();
		await browser.until(
			"!document.querySelector('.settings-catalog-refresh')?.disabled && document.body.innerText.includes('模型目录已更新')",
			"手动更新完成",
		);
		assert.equal(requests, before + 1);
		assert.equal((await fetch(`${origin}/api/models/refresh`, { method: "POST", body: "{}" })).status, 403);
		const headers = {
			Origin: origin,
			"X-CST-Web-Request": "1",
			"Content-Type": "application/json",
			"Idempotency-Key": "refresh-replay",
		};
		const post = (body = "{}") => fetch(`${origin}/api/models/refresh`, { method: "POST", headers, body });
		const once = requests;
		assert.equal((await post()).status, 200);
		assert.equal((await post()).status, 200);
		assert.equal(requests, once + 1, "去重键重放不重复联网");
		assert.equal((await post('{"force":true}')).status, 400);
		await browser.navigate(`${origin}/settings/models`);
		await browser.until("document.body.innerText.includes('catalog-one')", "已有模型");
		ids = ["catalog-one", "catalog-two"];
		await catalog.refresh({ force: true });
		await browser.until("document.body.innerText.includes('catalog-two')", "后台更新经 SSE 自动出现新模型");
		await browser.navigate(`${origin}/settings/add`);
		await browser.evaluate(
			`fetch("/api/settings",{method:"PATCH",headers:{"Content-Type":"application/json","X-CST-Web-Request":"1","Idempotency-Key":crypto.randomUUID()},body:JSON.stringify({theme:"dark"})})`,
		);
		await browser.until("document.documentElement.dataset.theme==='dark'", "深色主题生效");
		await browser.evaluate("document.fonts.ready");
		await browser.screenshot(join(root, "refresh-dark.png"));
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 390,
			height: 844,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await browser.click('button[aria-label="收起侧栏"]');
		await browser.until(
			"document.querySelector('.sidebar').getBoundingClientRect().right<=0 && !document.getAnimations().some(a=>a.playState==='running')",
			"窄屏侧栏收起",
		);
		await browser.screenshot(join(root, "refresh-mobile.png"));
		assert.equal(
			await browser.evaluate(
				"(()=>{const b=document.querySelector('.settings-catalog-refresh').getBoundingClientRect();return b.right<=innerWidth&&b.left>=0&&document.documentElement.scrollWidth<=innerWidth})()",
			),
			true,
		);
		fail = true;
		await browser.click(".settings-catalog-refresh");
		await browser.until(
			"!document.querySelector('.settings-catalog-refresh')?.disabled&&document.body.innerText.includes('已保留可用目录')",
			"失败提示与缓存保留",
		);
		const models = await (await fetch(`${origin}/api/models`)).json();
		assert.ok(models.models.some((model: { id: string }) => model.id === "catalog-two"));
	} finally {
		release?.();
		await browser?.close();
		await api.close();
		await pool.close();
		await catalog.close();
		server.closeAllConnections();
		upstream.closeAllConnections();
		await Promise.all([
			new Promise<void>((resolve) => server.close(() => resolve())),
			new Promise<void>((resolve) => upstream.close(() => resolve())),
		]);
		await rm(home, { recursive: true, force: true });
	}
});
