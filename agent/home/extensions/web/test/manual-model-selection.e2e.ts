import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

test("首次使用：默认模型身份、缺密钥弹窗、草稿返回、独立默认模型选择", { timeout: 90000 }, async (t) => {
	const oldKey = process.env.DEEPSEEK_API_KEY;
	delete process.env.DEEPSEEK_API_KEY;
	t.after(() => {
		if (oldKey === undefined) delete process.env.DEEPSEEK_API_KEY;
		else process.env.DEEPSEEK_API_KEY = oldKey;
	});
	const root = join(await testRoot(), "manual-model-ui");
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
			providers: {
				deepseek: { models: [model("deepseek-flash", "DeepSeek V4.1 Flash")] },
				probe: {
					api: "openai-completions",
					baseUrl: "http://127.0.0.1:9/v1",
					apiKey: "test-only",
					models: [model("one", "测试模型一"), model("two", "测试模型二")],
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
	const port = await freePort(),
		origin = `http://127.0.0.1:${port}`;
	const api = createWebApi(pool, home, port);
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions: [], stage: "manual-model-ui" }),
		api,
	);
	await listenWebServer(server, port);
	let browser: BrowserProbe | undefined;
	try {
		browser = await BrowserProbe.launch(root);
		await browser.navigate(`${origin}/`);
		await browser.until("!!document.querySelector('.model-key-dialog[open]')", "首次密钥提示");
		assert.equal(
			await browser.evaluate(
				"document.querySelector('.composer-model').textContent.includes('DeepSeek V4.1 Flash')",
			),
			true,
		);
		assert.equal(
			await browser.evaluate("document.querySelector('.composer-model').textContent.includes('自动选择')"),
			false,
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
		await browser.click(".model-key-close");
		await browser.fill(".composer-input", "检查电脑运行缓慢的原因");
		await browser.click(".composer-send");
		await browser.until(
			"document.querySelector('.model-key-dialog')?.textContent.includes('草稿已保留')",
			"发送前拦截",
		);
		assert.equal(pool.snapshot().length, 0, "没有创建空会话");
		await browser.click(".model-key-configure");
		await browser.until(
			"location.pathname==='/settings/provider'&&!!document.querySelector('.settings-key-input input')",
			"进入DeepSeek配置",
		);
		await browser.fill(".settings-key-input input", "test-placeholder-no-network");
		await browser.call("Input.dispatchKeyEvent", { type: "keyDown", key: "Enter", code: "Enter" });
		await browser.until(
			"location.pathname==='/'&&document.querySelector('.composer-input')?.textContent.includes('检查电脑')&&!document.querySelector('.model-key-dialog')",
			"配置后返回并保留草稿",
		);
		assert.equal(pool.snapshot().length, 0, "配置成功不会自动发送");
		await browser.navigate(`${origin}/settings/models`);
		await browser.until("!!document.querySelector('.settings-default-trigger')", "独立默认模型控件");
		assert.equal(
			await browser.evaluate("document.querySelector('.settings-default-trigger').textContent.trim()"),
			"DeepSeek V4.1 Flash",
			"默认模型控件只显示模型名",
		);
		assert.equal(
			await browser.evaluate(
				"Math.round(document.querySelector('.settings-default-trigger').getBoundingClientRect().width)",
			),
			208,
			"默认模型选择框使用紧凑宽度",
		);
		assert.equal(
			await browser.evaluate(
				"(()=>{const n=document.querySelector('.settings-provider-select .select__trigger'),r=n.getBoundingClientRect(),icon=n.querySelector('svg').getBoundingClientRect();return r.width>=73&&r.width<=76&&Math.abs(r.right-icon.right-12)<2})()",
			),
			true,
			"全部筛选保持紧凑，箭头距右12px",
		);
		await browser.screenshot(join(root, "model-preferences-compact.png"));
		for (const theme of ["light", "dark"]) {
			await browser.evaluate(`document.documentElement.dataset.theme=${JSON.stringify(theme)}`);
			await browser.click(".settings-default-trigger");
			await browser.until("!!document.querySelector('.settings-default-popover')", "默认模型选单");
			assert.equal(
				await browser.evaluate(
					"document.querySelector('.settings-default-option small').textContent.includes('DeepSeek')",
				),
				true,
				"提供商保留在下拉选项",
			);
			await browser.evaluate(
				"Promise.all(document.getAnimations().filter(a=>a.effect?.getComputedTiming().iterations!==Infinity).map(a=>a.finished.catch(()=>{})))",
			);
			await browser.screenshot(join(root, `default-model-${theme}.png`));
			await browser.call("Input.dispatchKeyEvent", { type: "keyDown", key: "Escape", code: "Escape" });
			await browser.until("!document.querySelector('.settings-default-popover')", "选单收起");
		}
		await browser.click(".settings-default-trigger");
		await browser.fill(".settings-default-popover input", "测试模型二");
		await browser.until("document.querySelectorAll('.settings-default-option').length===1", "搜索默认模型");
		await browser.click(".settings-default-option");
		await browser.until(
			"document.querySelector('.settings-default-trigger')?.textContent.includes('测试模型二')",
			"保存默认模型",
		);
		const settings = JSON.parse(await readFile(join(home, "settings.json"), "utf8"));
		assert.equal(settings.defaultProvider, "probe");
		assert.equal(settings.defaultModel, "two");
		await browser.navigate(`${origin}/`);
		await browser.until(
			"document.querySelector('.composer-model')?.textContent.includes('测试模型二')",
			"首页使用保存的默认值",
		);
		await browser.click(".composer-model");
		await browser.until("!!document.querySelector('.model-popover')", "手动模型列表");
		assert.equal(
			await browser.evaluate("document.querySelector('.model-popover').textContent.includes('自动选择')"),
			false,
		);
		await browser.click('[role="option"][data-key="probe/one"]');
		await browser.until(
			"document.querySelector('.composer-model')?.textContent.includes('测试模型一')",
			"手动选择当前模型",
		);
		assert.equal(JSON.parse(await readFile(join(home, "settings.json"), "utf8")).defaultModel, "two");
	} finally {
		await browser?.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await api.close();
		await pool.close();
		await rm(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
	}
});
