import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

/** 完全没有配置模型时的首次提示：24 小时一次，并提供 CSTOA 登录与 APIKEY 两条路径。 */
test("首次密钥提示：无凭据才自动弹窗，24 小时内不再出现", { timeout: 90000 }, async (t) => {
	const oldKey = process.env.DEEPSEEK_API_KEY;
	delete process.env.DEEPSEEK_API_KEY;
	t.after(() => {
		if (oldKey === undefined) delete process.env.DEEPSEEK_API_KEY;
		else process.env.DEEPSEEK_API_KEY = oldKey;
	});
	const root = join(await testRoot(), "model-key-prompt");
	await mkdir(root, { recursive: true });
	const home = await mkdtemp(join(root, "home-"));
	const model = (id: string, name: string) => ({
		id,
		name,
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
			providers: { deepseek: { models: [model("deepseek-flash", "DeepSeek V4.1 Flash")] } },
		}),
	);
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	const port = await freePort(),
		origin = `http://127.0.0.1:${port}`;
	const api = createWebApi(pool, home, port);
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions: [], stage: "model-key-prompt" }),
		api,
	);
	await listenWebServer(server, port);
	const dialogText = "document.querySelector('.model-key-dialog')?.textContent ?? ''";
	const dialogOpen = "!!document.querySelector('.model-key-dialog[open]')";
	let browser: BrowserProbe | undefined;
	try {
		browser = await BrowserProbe.launch(root);
		await browser.navigate(`${origin}/`);
		await browser.until(dialogOpen, "无凭据时首次提示");
		assert.equal(await browser.evaluate(`(${dialogText}).includes('尚未配置模型')`), true, "标题说明尚未配置模型");
		assert.equal(
			await browser.evaluate(`(${dialogText}).includes('登录 CSTOA')&&(${dialogText}).includes('APIKEY')`),
			true,
			"文案同时引导 CSTOA 登录与 APIKEY",
		);
		assert.equal(
			await browser.evaluate(
				`[...document.querySelectorAll('.model-key-dialog .exit-actions button')].map(b=>b.textContent.trim()).join('/')`,
			),
			"暂不配置/填写 APIKEY/登录 CSTOA",
			"三条操作固定顺序",
		);
		assert.equal(
			await browser.evaluate(`Number(localStorage.getItem('cst-model-key-prompt-at'))>0`),
			true,
			"记录提示时间",
		);
		for (const theme of ["light", "dark"]) {
			await browser.evaluate(`document.documentElement.dataset.theme=${JSON.stringify(theme)}`);
			await browser.evaluate(
				"Promise.all(document.getAnimations().filter(a=>a.effect?.getComputedTiming().iterations!==Infinity).map(a=>a.finished.catch(()=>{})))",
			);
			await browser.screenshot(join(root, `missing-key-${theme}.png`));
		}
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 390,
			height: 844,
			deviceScaleFactor: 1,
			mobile: false,
		});
		assert.equal(
			await browser.evaluate(
				"(()=>{const r=document.querySelector('.model-key-dialog').getBoundingClientRect();return r.left>=23&&r.right<=innerWidth-23})()",
			),
			true,
			"窄屏左右各留 24",
		);
		await browser.evaluate(
			"Promise.all(document.getAnimations().filter(a=>a.effect?.getComputedTiming().iterations!==Infinity).map(a=>a.finished.catch(()=>{})))",
		);
		await browser.screenshot(join(root, "missing-key-mobile.png"));
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 1440,
			height: 1000,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await browser.evaluate("document.documentElement.dataset.theme='light'");
		await browser.click(".model-key-dialog .exit-actions button:first-child");
		await browser.until(`!${dialogOpen}`, "暂不配置关闭弹窗");
		await browser.navigate(`${origin}/`);
		await browser.until(
			"document.querySelector('.composer-model')?.textContent.includes('DeepSeek V4.1 Flash')",
			"刷新后加载默认模型",
		);
		assert.equal(await browser.evaluate(dialogOpen), false, "24 小时内刷新不再自动提示");
		await browser.evaluate("localStorage.setItem('cst-model-key-prompt-at',String(Date.now()-25*60*60*1000))");
		await browser.navigate(`${origin}/`);
		await browser.until(dialogOpen, "超过 24 小时重新提示");
		await browser.click(".model-key-dialog .exit-actions button:nth-child(2)");
		await browser.until(
			"location.pathname==='/settings/provider'&&!!document.querySelector('.settings-key-input input')",
			"填写 APIKEY 进入服务配置",
		);
		assert.equal(
			new URL(await browser.evaluate("location.href")).searchParams.get("provider"),
			"deepseek",
			"APIKEY 入口指向当前模型服务",
		);
		await browser.navigate(`${origin}/`);
		await browser.evaluate("localStorage.removeItem('cst-model-key-prompt-at')");
		await browser.navigate(`${origin}/`);
		await browser.until(dialogOpen, "清空记录后重新提示");
		await browser.click(".model-key-dialog .exit-actions button:nth-child(3)");
		await browser.until("location.pathname==='/login'", "登录 CSTOA 进入登录页");
		assert.equal(
			new URL(await browser.evaluate("location.href")).searchParams.get("provider"),
			"cstoa",
			"登录入口指向 CSTOA",
		);
	} finally {
		await browser?.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await api.close();
		await pool.close();
		await rm(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
	}
});
