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

test("设置用户链路：分类导航、自动保存、失败回滚、凭据、多步骤授权、半屏与通知定位", { timeout: 120_000 }, async () => {
	const root = join(await testRoot(), "settings-ui");
	await mkdir(root, { recursive: true });
	const home = await mkdtemp(join(root, "home-"));
	await writeFile(
		join(home, "settings.json"),
		JSON.stringify({ enabledModels: ["probe/two", "probe/one"], theme: "light", defaultTools: ["read"] }),
	);
	await writeFile(
		join(home, "models.json"),
		JSON.stringify({
			providers: {
				probe: {
					baseUrl: "http://127.0.0.1:9/v1",
					api: "openai-completions",
					apiKey: "test-only",
					models: Array.from({ length: 8 }, (_, i) => ({
						id: ["one", "two"][i] ?? `model-${i}`,
						name: `设置测试模型 ${i + 1}`,
						reasoning: false,
						input: ["text"],
						contextWindow: 10000,
						maxTokens: 100,
					})),
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
		() => ({ sessions: [], stage: "settings-ui" }),
		api,
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
		assert.equal((await send("/api/auth/openai/api-key", "PUT", { key: "settings-initial-secret" })).status, 200);
		browser = await BrowserProbe.launch(root);
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 1920,
			height: 1080,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await browser.navigate(`${origin}/settings`);
		await browser.until(
			"document.querySelector('.settings-services')?.textContent.includes('OpenAI')",
			"已配置服务列表",
		);
		await browser.screenshot(join(root, "services-light.png"));
		await browser.click('a[href="/settings/add"]');
		await browser.until("!!document.querySelector('.settings-search input')", "添加服务");
		await browser.fill(".settings-search input", "anthropic");
		await browser.until("document.querySelectorAll('.settings-service-row').length===1", "服务搜索");
		await browser.screenshot(join(root, "add-service-light.png"));
		await browser.fill(".settings-search input", "this-provider-does-not-exist");
		await browser.until("document.body.innerText.includes('没有匹配的模型服务')", "服务搜索空状态");
		await browser.call("Page.reload");
		await browser.until(
			"document.querySelector('.settings-page-heading h2')?.textContent==='添加服务'",
			"添加服务深链接刷新",
		);
		await browser.click('a[href="/settings/models"]');
		await browser.until("document.querySelectorAll('.settings-model').length===6", "模型预览六项");
		await browser.click(".settings-provider-select button");
		await browser.until("!!document.querySelector('[role=option][data-key=probe]')", "服务筛选选项");
		await browser.click("[role=option][data-key=probe]");
		await browser.until(
			"[...document.querySelectorAll('.settings-model-provider')].every(e=>e.textContent==='probe')",
			"按服务筛选模型",
		);
		await browser.screenshot(join(root, "model-preferences-light.png"));
		await browser.click(".settings-models-toggle");
		await browser.fill(".settings-search input", "probe");
		await browser.until("document.querySelectorAll('.settings-model').length===8", "展开全部模型");
		await browser.evaluate(
			"document.querySelector('[aria-label=\"调整 设置测试模型 1 的顺序\"]').dispatchEvent(new KeyboardEvent('keydown',{key:'ArrowUp',altKey:true,bubbles:true}))",
		);
		await browser.until(
			"document.querySelector('.settings-notification')?.textContent.includes('已自动保存')",
			"键盘排序自动保存",
		);
		assert.deepEqual(JSON.parse(await readFile(join(home, "settings.json"), "utf8")).enabledModels, [
			"probe/one",
			"probe/two",
		]);
		await browser.fill(".settings-search input", "设置测试模型 3");
		await browser.until("document.querySelectorAll('.settings-model').length===1", "模型搜索");
		await browser.click(".settings-check-box");
		await browser.until(
			"document.querySelector('.settings-check-box input')?.checked && !document.querySelector('.settings-model-card')?.getAttribute('aria-busy')?.includes('true')",
			"模型启用保存",
		);
		assert.deepEqual(JSON.parse(await readFile(join(home, "settings.json"), "utf8")).enabledModels, [
			"probe/one",
			"probe/two",
			"probe/model-2",
		]);
		// 只在 HTTP 边界注入一次失败，产品状态与持久化继续走真实接口。
		await browser.evaluate(
			"window.__settingsFetch=window.fetch;window.__settingsFailed=false;window.fetch=async(...args)=>{if(!window.__settingsFailed&&String(args[0]).endsWith('/api/models/scoped')&&args[1]?.method==='POST'){window.__settingsFailed=true;return new Response(JSON.stringify({error:{message:'测试写入失败'}}),{status:503,headers:{'Content-Type':'application/json'}})}return window.__settingsFetch(...args)}",
		);
		await browser.click(".settings-check-box");
		await browser.until(
			"document.querySelector('.settings-load-error')?.textContent.includes('测试写入失败') && document.querySelector('.settings-check-box input')?.checked",
			"保存失败恢复勾选",
		);
		await browser.evaluate("window.fetch=window.__settingsFetch");
		await browser.click('a[href="/settings/general"]');
		await browser.until("!!document.querySelector('[aria-label=深色]')", "主题图标选择器");
		await browser.click('[aria-label="深色"]');
		await browser.until(
			"document.documentElement.dataset.theme==='dark' && !!document.querySelector('.settings-notification')",
			"主题自动保存",
		);
		assert.equal(JSON.parse(await readFile(join(home, "web-settings.json"), "utf8")).theme, "dark");
		assert.equal(JSON.parse(await readFile(join(home, "settings.json"), "utf8")).theme, "light", "Web主题不修改TUI");
		assert.equal(
			await browser.evaluate(
				"(()=>{const main=document.querySelector('.workspace-content').getBoundingClientRect(),toast=document.querySelector('.settings-notification').getBoundingClientRect();return toast.left>=main.left&&toast.right<=main.right})()",
			),
			true,
			"保存通知不遮挡侧栏或超出主区",
		);
		await browser.evaluate("Promise.allSettled(document.getAnimations().map(a=>a.finished))");
		assert.equal(
			await browser.evaluate("document.querySelectorAll('.settings-notification').length"),
			1,
			"连续保存复用同一条通知",
		);
		await browser.screenshot(join(root, "general-dark.png"));
		await browser.evaluate(
			"window.__themeFetch=window.fetch;window.fetch=async(...args)=>String(args[0]).endsWith('/api/settings')&&args[1]?.method==='PATCH'?new Response(JSON.stringify({error:{message:'测试主题保存失败'}}),{status:503,headers:{'Content-Type':'application/json'}}):window.__themeFetch(...args)",
		);
		await browser.click('[aria-label="浅色"]');
		await browser.until(
			"document.documentElement.dataset.theme==='dark' && document.querySelector('.settings-load-error')?.textContent.includes('测试主题保存失败')",
			"主题失败恢复已保存主题",
		);
		await browser.evaluate("window.fetch=window.__themeFetch");
		await browser.navigate(`${origin}/settings/provider?provider=openai&method=api_key`);
		await browser.until("!!document.querySelector('.settings-key-input input')", "密钥管理页");
		assert.equal(
			await browser.evaluate("document.querySelector('.settings-key-input input').value"),
			"",
			"不读取已有密钥",
		);
		await browser.screenshot(join(root, "provider-dark.png"));
		await browser.fill(".settings-key-input input", "settings-updated-secret");
		await browser.click(".settings-page-heading");
		await browser.until(
			"!!document.querySelector('.settings-notification') && !!document.querySelector('.settings-key-input input')",
			"密钥失焦保存",
		);
		assert.equal(await browser.evaluate("document.querySelector('.settings-key-input input').value"), "");
		const auth = await readFile(join(home, "auth.json"), "utf8");
		assert.ok(auth.includes("settings-updated-secret"));
		assert.equal(
			await browser.evaluate(
				"JSON.stringify({...localStorage,...sessionStorage}).includes('settings-updated-secret')",
			),
			false,
		);
		await browser.click(".settings-danger-zone button");
		await browser.until("!!document.querySelector('dialog[open]')", "移除凭据确认");
		assert.equal(await browser.evaluate("document.activeElement.textContent"), "取消");
		await browser.click("dialog .exit-confirm");
		await browser.until(
			"!document.querySelector('dialog[open]') && !document.querySelector('.settings-danger-zone')",
			"移除凭据完成",
		);
		assert.ok(!(await readFile(join(home, "auth.json"), "utf8")).includes("settings-updated-secret"));
		await browser.navigate(`${origin}/settings/provider?provider=cloudflare-workers-ai&method=api_key`);
		await browser.until("!!document.querySelector('.settings-key-input input')", "多步骤密钥服务");
		await browser.fill(".settings-key-input input", "settings-cloudflare-secret");
		await browser.click(".settings-page-heading");
		await browser.until(
			"document.querySelector('.oauth-prompt')?.textContent.includes('account ID')",
			"Pi额外请求账号ID",
		);
		await browser.fill(".oauth-prompt input", "settings-test-account");
		await browser.click('.oauth-prompt button[type="submit"]');
		await browser.until(
			"!!document.querySelector('.settings-key-input input') && !!document.querySelector('.settings-danger-zone')",
			"多步骤授权完成",
		);
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 960,
			height: 1080,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await browser.navigate(`${origin}/settings`);
		await browser.until("!!document.querySelector('.settings-services')", "半屏服务列表");
		await browser.evaluate("document.querySelector('[aria-label=展开侧栏]')?.click()");
		await browser.until(
			"(()=>{const sidebar=document.querySelector('.sidebar').getBoundingClientRect(),main=document.querySelector('.workspace-content').getBoundingClientRect();return sidebar.width>0&&main.left>=sidebar.right-1&&!document.querySelector('.sidebar-backdrop')})()",
			"半屏侧栏保留占位且不遮挡主区",
		);
		assert.equal(
			await browser.evaluate("Math.max(document.documentElement.scrollWidth,document.body.scrollWidth)<=innerWidth"),
			true,
		);
		await browser.screenshot(join(root, "services-narrow-dark.png"));
		await browser.click('a[href="/settings/general"]');
		await browser.until("!!document.querySelector('[aria-label=浅色]')", "半屏主题页");
		await browser.click('[aria-label="浅色"]');
		await browser.until("!!document.querySelector('.settings-notification')", "半屏自动保存通知");
		assert.equal(
			await browser.evaluate(
				"(()=>{const r=document.querySelector('.settings-notification').getBoundingClientRect();return r.left>=0&&r.right<=innerWidth})()",
			),
			true,
			"半屏保存通知不越界",
		);
		await browser.screenshot(join(root, "general-narrow-light.png"));
		await browser.click('[aria-label="收起侧栏"]');
		await browser.until("document.querySelector('.sidebar').inert", "侧栏收起后隐藏交互");
		await browser.click('a[href="/settings/shortcuts"]');
		await browser.until("document.querySelectorAll('.settings-shortcuts kbd').length>0", "快捷键导航");
		await browser.evaluate("history.back()");
		await browser.until("location.pathname==='/settings/general'", "浏览器返回");
		await browser.screenshot(join(root, "general-narrow-collapsed.png"));
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 480,
			height: 900,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await browser.click('[aria-label="展开侧栏"]');
		await browser.until("!!document.querySelector('.sidebar-backdrop')", "手机宽度设置侧栏为浮层");
		assert.equal(await browser.evaluate("document.querySelector('.workspace-content').getBoundingClientRect().x"), 0);
		assert.equal(
			await browser.evaluate(
				"document.querySelector('.workspace-content').getBoundingClientRect().width>=innerWidth-1",
			),
			true,
			"浮层不挤占主区宽度",
		);
		assert.equal(
			await browser.evaluate("document.documentElement.scrollWidth<=innerWidth"),
			true,
			"手机设置页不横向溢出",
		);
		await browser.screenshot(join(root, "general-mobile-overlay.png"));
		await browser.evaluate("document.querySelector('.sidebar-backdrop').click()");
		await browser.until(
			"!!document.querySelector('[aria-label=展开侧栏]') && !document.querySelector('.sidebar-backdrop')",
			"点击遮罩收起侧栏",
		);
	} finally {
		await browser?.close();
		await api.close();
		await pool.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await rm(home, { recursive: true, force: true });
	}
});
