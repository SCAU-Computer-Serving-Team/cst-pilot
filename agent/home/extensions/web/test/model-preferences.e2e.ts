import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { configureTestModel, testRoot } from "./support.ts";

test("模型偏好：勾选自动保存、失败回滚、只读刷新与保存期间更换提供商", { timeout: 60_000 }, async () => {
	const root = join(await testRoot(), "model-preferences");
	await mkdir(root, { recursive: true });
	const home = await mkdtemp(join(root, "home-"));
	await configureTestModel(home);
	await writeFile(join(home, "web-settings.json"), JSON.stringify({ theme: "light" }));
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	const port = await freePort();
	const origin = `http://127.0.0.1:${port}`;
	const api = createWebApi(pool, home, port);
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions: [], stage: "model-preferences" }),
		api,
	);
	const services = await pool.getServices();
	const available = await services.modelRuntime.getAvailable();
	services.settingsManager.setEnabledModels(
		available.filter((model) => model.provider !== "test-probe").map((model) => `${model.provider}/${model.id}`),
	);
	await listenWebServer(server, port);
	const send = (path: string, method: string, body: unknown) =>
		fetch(`${origin}${path}`, {
			method,
			headers: {
				Origin: origin,
				"Content-Type": "application/json",
				"X-CST-Web-Request": "1",
				"Idempotency-Key": crypto.randomUUID(),
			},
			body: JSON.stringify(body),
		});
	let browser: BrowserProbe | undefined;
	try {
		browser = await BrowserProbe.launch(root);
		await browser.navigate(`${origin}/settings/models`);
		await browser.until("!!document.querySelector('.settings-check-box')", "模型列表");
		await browser.fill(".settings-search input", "test-probe");
		await browser.until("document.querySelectorAll('.settings-model').length===1", "最后一个待启用模型");
		// 只在 HTTP 边界延迟读取与写入响应，复选框、缓存、SSE 和持久化均走产品实现。
		await browser.evaluate(`
			window.__posts=[]; window.__reads=0; window.__readWaiters=[]; window.__holdReads=true;
			window.__originalFetch=window.fetch;
			window.fetch=async(...args)=>{
				const path=String(args[0]), save=path.endsWith('/api/models/scoped')&&args[1]?.method==='POST';
				if(save&&window.__failSave){window.__failSave=false;return new Response(JSON.stringify({error:{message:'测试保存失败'}}),{status:503,headers:{'Content-Type':'application/json'}})}
				if(path.endsWith('/api/models')&&(!args[1]?.method||args[1].method==='GET')){window.__reads++;if(window.__holdReads)await new Promise(resolve=>window.__readWaiters.push(resolve))}
				const response=await window.__originalFetch(...args);
				if(save){window.__posts.push(await response.clone().json());if(window.__holdSave)await new Promise(resolve=>window.__releaseSave=resolve)}
				return response;
			};
		`);
		await browser.click(".settings-check-box");
		await browser.until(
			"window.__posts.length===1 && document.querySelector('.settings-model-card')?.getAttribute('aria-busy')==='false'",
			"勾选保存完成",
		);
		assert.equal(await browser.evaluate("document.querySelector('.settings-check-box input')?.checked"), true);
		assert.deepEqual(await browser.evaluate("window.__posts[0]"), { enabled: null });
		await browser.evaluate("window.__holdReads=false;window.__readWaiters.splice(0).forEach(resolve=>resolve())");
		await browser.until("document.querySelector('.settings-check-box input')?.checked", "后台同步保留勾选");

		await browser.click(".settings-check-box");
		await browser.until(
			"window.__posts.length===2 && document.querySelector('.settings-model-card')?.getAttribute('aria-busy')==='false'",
			"取消启用已保存",
		);
		const before = await browser.evaluate<number>("window.__reads");
		const saved = await readFile(join(home, "settings.json"), "utf8");
		assert.equal(
			await browser.evaluate(
				"document.querySelector('.settings-model-bulk button:nth-child(3)').textContent.trim()",
			),
			"刷新",
		);
		await browser.click(".settings-model-bulk button:nth-child(3)");
		await browser.until(
			`window.__reads>${before} && !document.querySelector('.settings-check-box input')?.checked`,
			"刷新保留启用范围",
		);
		assert.equal(await browser.evaluate("window.__posts.length"), 2, "刷新不提交范围写入");
		assert.equal(await readFile(join(home, "settings.json"), "utf8"), saved);

		await browser.evaluate("window.__failSave=true");
		await browser.click(".settings-check-box");
		await browser.until(
			"document.querySelector('.settings-load-error')?.textContent.includes('测试保存失败') && !document.querySelector('.settings-check-box input')?.checked",
			"失败恢复已确认勾选",
		);
		await browser.click(".settings-check-box");
		await browser.until(
			"window.__posts.length===3 && document.querySelector('.settings-model-card')?.getAttribute('aria-busy')==='false'",
			"失败后可重新勾选",
		);

		await browser.fill(".settings-search input", "");
		await browser.click(".settings-provider-select button");
		await browser.click("[role=option][data-key=test-probe]");
		await browser.until("document.querySelectorAll('.settings-model').length===1", "提供商筛选");
		await browser.evaluate("window.__holdSave=true;window.__holdReads=true");
		await browser.click(".settings-check-box");
		await browser.until("typeof window.__releaseSave==='function'", "保存响应暂缓");
		const readCount = await browser.evaluate<number>("window.__reads");
		assert.equal(
			(
				await send("/api/auth/custom", "POST", {
					id: "replacement-probe",
					name: "替换后的服务",
					baseUrl: "http://127.0.0.1:9/v1",
					api: "openai-completions",
					apiKey: "test-only",
					models: [
						{
							id: "new-model",
							name: "替换后的模型",
							reasoning: false,
							image: false,
							contextWindow: 10000,
							maxTokens: 100,
						},
					],
				})
			).status,
			200,
		);
		const custom = await (await fetch(`${origin}/api/auth/custom/test-probe`)).json();
		assert.equal((await send("/api/auth/custom/test-probe", "DELETE", { revision: custom.revision })).status, 200);
		await browser.until(`window.__reads>${readCount}`, "提供商变化触发自动读取");
		await browser.evaluate("window.__holdSave=false;window.__releaseSave()");
		await browser.until(
			"document.querySelector('.settings-model-card')?.getAttribute('aria-busy')==='false'",
			"保存响应完成",
		);
		await browser.evaluate("window.__holdReads=false;window.__readWaiters.splice(0).forEach(resolve=>resolve())");
		await browser.until(
			"document.querySelector('.settings-models')?.textContent.includes('替换后的模型') && document.querySelector('.settings-provider-select')?.textContent.includes('全部')",
			"保存期间更换服务后自动刷新并重置失效筛选",
		);
		assert.equal(
			await browser.evaluate(
				"[...document.querySelectorAll('.settings-model-provider')].some(node=>node.textContent==='test-probe')",
			),
			false,
		);
		await browser.screenshot(join(root, "model-preferences-refreshed.png"));
	} finally {
		await browser?.close();
		await api.close();
		await pool.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await rm(home, { recursive: true, force: true });
	}
});
