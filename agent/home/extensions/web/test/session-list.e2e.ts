import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm, utimes } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { configureTestModel, testRoot } from "./support.ts";

test("最近二十项、16px 字号、四组全部会话与共享批量管理", { timeout: 90000 }, async (context) => {
	const root = join(await testRoot(), "session-list");
	await mkdir(root, { recursive: true });
	const home = await mkdtemp(join(root, "home-"));
	await configureTestModel(home);
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	const now = Date.now();
	context.mock.timers.enable({ apis: ["Date"], now });
	for (let index = 0; index < 24; index++) {
		const date = new Date(now);
		date.setDate(date.getDate() - (index === 23 ? 45 : index));
		context.mock.timers.setTime(date.getTime());
		const slot = await pool.create();
		slot.session.sessionManager.appendMessage({
			role: "user",
			content: [{ type: "text", text: `会话 ${index}` }],
			timestamp: Date.now(),
		});
		slot.session.sessionManager.appendMessage({
			role: "assistant",
			content: [{ type: "text", text: "已保存" }],
			api: "openai-completions",
			provider: "test-probe",
			model: "mock",
			usage: {
				input: 0,
				output: 0,
				cacheRead: 0,
				cacheWrite: 0,
				totalTokens: 0,
				cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
			},
			stopReason: "stop",
			timestamp: Date.now(),
		});
		slot.session.setSessionName(
			index === 23 ? "用于验证长标题截断的会话名称，包含中文与 English 以及更多文字" : `会话 ${index}`,
		);
		assert.ok(slot.session.sessionFile);
		await utimes(slot.session.sessionFile, date, date);
	}
	context.mock.timers.reset();
	const expected = (await pool.list()).sort((a, b) => Date.parse(b.updatedAt ?? "") - Date.parse(a.updatedAt ?? ""));
	const port = await freePort();
	const api = createWebApi(pool, home, port);
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		async () => ({ sessions: await pool.list(), stage: "test" }),
		api,
	);
	await listenWebServer(server, port);
	let browser: BrowserProbe | undefined;
	try {
		browser = await BrowserProbe.launch(root);
		const probe = browser;
		const origin = `http://127.0.0.1:${port}`;
		async function settle() {
			await probe.evaluate(`(async () => {
				await document.fonts.ready;
				await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
				await Promise.all(document.querySelector('.sidebar-recent').getAnimations({subtree:true}).map(a => a.finished.catch(() => {})));
			})()`);
		}
		async function screenshot(name: string) {
			await settle();
			await probe.screenshot(join(root, name));
		}
		await probe.call("Emulation.setDeviceMetricsOverride", {
			width: 1920,
			height: 1080,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await probe.call("Page.navigate", { url: `${origin}/#cst-web-start` });
		await probe.until("document.querySelectorAll('.sidebar a.sidebar-session').length === 20", "最近二十项");
		assert.deepEqual(
			await probe.evaluate(
				"[...document.querySelectorAll('.sidebar a.sidebar-session')].map(e=>e.dataset.sessionId)",
			),
			expected.slice(0, 20).map((row) => row.id),
		);
		assert.equal(
			await probe.evaluate("document.querySelector('.sidebar-recent-heading').textContent.trim()"),
			"最近",
		);
		assert.equal(
			await probe.evaluate("document.querySelectorAll('.sidebar input[type=checkbox]').length"),
			0,
			"常态不暴露隐藏勾选框",
		);
		await probe.until("!document.querySelector('[data-startup-rows]')", "入场结束");
		const sizes = await probe.evaluate<{ nav: number; row: number; font: string; model: string }>(
			`(() => ({ nav: document.querySelector('.sidebar-new').getBoundingClientRect().height, row: document.querySelector('.sidebar-session').getBoundingClientRect().height, font: getComputedStyle(document.querySelector('.sidebar-session')).fontSize, model: getComputedStyle(document.querySelector('.composer-model')).fontSize }))()`,
		);
		assert.deepEqual(sizes, { nav: 40, row: 32, font: "16px", model: "13px" });
		await screenshot("home-light.png");
		await probe.click(".sidebar-recent-heading");
		await probe.until(
			"document.querySelector('.sidebar-recent-heading').getAttribute('aria-expanded') === 'false'",
			"收起最近会话",
		);
		assert.equal(await probe.evaluate("document.querySelector('.sidebar-recent .disclosure__content').inert"), true);
		await probe.click(".sidebar-all");
		await probe.until(
			"location.pathname === '/sessions' && document.querySelectorAll('.session-list-item').length === 24",
			"全部会话独立页面",
		);
		assert.deepEqual(
			await probe.evaluate("[...document.querySelectorAll('.session-list-group h2')].map(e=>e.textContent)"),
			["今天", "最近一周", "最近一个月", "更早"],
		);
		assert.equal(
			await probe.evaluate("getComputedStyle(document.querySelector('.session-list .sidebar-session')).fontSize"),
			"16px",
		);
		await probe.click(".session-list-manage");
		await probe.click(".session-list-toolbar .session-management-toolbar button:first-of-type");
		assert.equal(
			await probe.evaluate("document.querySelectorAll('.session-list input:checked').length"),
			24,
			"全选覆盖完整列表",
		);
		await probe.click(".session-list-toolbar .session-management-toolbar button:first-of-type");
		assert.equal(await probe.evaluate("document.querySelectorAll('.session-list input:checked').length"), 0);
		const target = expected[0];
		await probe.click(`.session-list [data-session-id="${target.id}"]`);
		await probe.click(".session-list-delete");
		await probe.until("!!document.querySelector('.session-delete-dialog[open]')", "删除确认");
		await probe.click(".session-delete-dialog .exit-confirm");
		await probe.until(
			"document.querySelectorAll('.session-list-item').length === 23 && !!document.querySelector('.session-delete-notice')",
			"列表立即同步删除",
		);
		assert.ok((await pool.list()).every((row) => row.id !== target.id));
		assert.deepEqual(
			await probe.evaluate(
				"[...document.querySelectorAll('.sidebar a.sidebar-session')].map(e=>e.dataset.sessionId)",
			),
			expected.slice(1, 21).map((row) => row.id),
			"全部会话删除最近项后，侧栏同步补齐二十项",
		);
		await probe.navigate(`${origin}/sessions`);
		await probe.until("document.querySelectorAll('.session-list-item').length === 23", "刷新直达全部会话");
		const selectionAligned = `(() => { const selection = document.querySelector('.sidebar-selection').getBoundingClientRect(); const link = document.querySelector('.sidebar-all').getBoundingClientRect(); return Math.abs(selection.top - link.top) < 1 && Math.abs(selection.height - link.height) < 1; })()`;
		await settle();
		await probe.until(selectionAligned, "展开后全部会话选中背景对齐");
		await probe.click(".sidebar-recent-heading");
		await settle();
		await probe.until(selectionAligned, "折叠后全部会话选中背景对齐");
		await probe.click(".sidebar-recent-heading");
		await settle();
		await probe.until(selectionAligned, "重新展开后选中背景对齐");
		assert.match(
			await probe.evaluate<string>(
				"getComputedStyle(document.querySelector('.session-list .sidebar-session')).fontFamily",
			),
			/CST UI Sans/,
			"全部会话沿用工作区字体",
		);
		await screenshot("all-light.png");
		await probe.evaluate("document.documentElement.dataset.theme='dark'");
		await screenshot("all-dark.png");
		await probe.click("a.sidebar-new");
		await probe.until("location.pathname === '/' && !!document.querySelector('.composer-model')", "返回主页");
		if (
			await probe.evaluate(
				"document.querySelector('.sidebar-recent-heading').getAttribute('aria-expanded') === 'false'",
			)
		)
			await probe.click(".sidebar-recent-heading");
		await probe.click(".sidebar a.sidebar-session");
		await probe.until(
			"location.pathname.startsWith('/s/') && !!document.querySelector('.chat-loaded') && document.querySelector('.sidebar-selection').style.height === '32px'",
			"会话选中背景适配高度",
		);
		await probe.click(".sidebar-recent-heading");
		await probe.until(
			"document.querySelector('.sidebar-recent-heading').getAttribute('aria-expanded') === 'false'",
			"最近会话完成折叠状态提交",
		);
		assert.equal(
			await probe.evaluate("document.querySelector('.sidebar-selection').style.visibility"),
			"hidden",
			"折叠后不残留选中背景",
		);
		await probe.click(".sidebar-recent-heading");
		await probe.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "reduce" }],
		});
		await probe.click("a.sidebar-new");
		await screenshot("home-dark.png");
		for (const width of [960, 390]) {
			await probe.call("Emulation.setDeviceMetricsOverride", {
				width,
				height: 900,
				deviceScaleFactor: 1,
				mobile: false,
			});
			await probe.navigate(`${origin}/sessions`);
			await probe.until("document.querySelectorAll('.session-list-item').length === 23", "窄屏列表");
			if (await probe.evaluate("!!document.querySelector('.sidebar-restore')"))
				await probe.click(".sidebar-restore");
			await screenshot(`all-${width}.png`);
			await probe.click(".sidebar-all");
			await probe.until("document.querySelector('.sidebar').inert", "窄屏导航收起侧栏");
			await screenshot(`all-${width}-content.png`);
			await probe.click(".session-list-manage");
			await probe.evaluate("document.querySelector('.session-list input').focus()");
			await probe.call("Input.dispatchKeyEvent", {
				type: "keyDown",
				key: " ",
				code: "Space",
				windowsVirtualKeyCode: 32,
			});
			await probe.call("Input.dispatchKeyEvent", {
				type: "keyUp",
				key: " ",
				code: "Space",
				windowsVirtualKeyCode: 32,
			});
			assert.equal(await probe.evaluate("document.querySelectorAll('.session-list input:checked').length"), 1);
			await screenshot(`all-${width}-managing.png`);
			await probe.click(".session-list-toolbar .session-management-toolbar button:last-of-type");
			assert.equal(
				await probe.evaluate("document.documentElement.scrollWidth > innerWidth"),
				false,
				"窄屏没有横向溢出",
			);
		}
		const fixture = await probe.call<{ identifier: string }>("Page.addScriptToEvaluateOnNewDocument", {
			source: `window.__listFixture = 'error';
			const fetchOriginal = window.fetch;
			window.fetch = (...args) => String(args[0]).endsWith('/api/state') && window.__listFixture !== 'normal'
				? Promise.resolve(new Response(JSON.stringify(window.__listFixture === 'error' ? {error:'列表读取失败测试'} : {sessions:[],stage:'test'}), {status:window.__listFixture === 'error' ? 503 : 200,headers:{'Content-Type':'application/json'}}))
				: fetchOriginal(...args);`,
		});
		await probe.navigate(`${origin}/sessions`);
		await probe.until(
			"document.querySelector('.session-list-error')?.textContent.includes('503')",
			"读取错误与重试入口",
		);
		assert.equal(
			await probe.evaluate("!!document.querySelector('.session-list-empty')"),
			false,
			"错误时不继续显示加载状态",
		);
		await probe.evaluate("window.__listFixture = 'empty'");
		await probe.click(".session-list-error button");
		await probe.until("document.querySelector('.session-list-empty')?.textContent === '暂无会话'", "重试后空列表");
		assert.equal(await probe.evaluate("document.querySelector('.session-list-manage').disabled"), true);
		await probe.call("Page.removeScriptToEvaluateOnNewDocument", { identifier: fixture.identifier });
		await probe.navigate(`${origin}/sessions`);
		await probe.until("document.querySelectorAll('.session-list-item').length === 23", "恢复真实列表");
	} finally {
		await browser?.close();
		await api.close();
		await pool.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await rm(home, { recursive: true, force: true });
	}
});
