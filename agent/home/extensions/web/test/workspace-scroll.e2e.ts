import assert from "node:assert/strict";
import { mkdir, mkdtemp } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { configureTestModel, testRoot } from "./support.ts";

test("工作台侧栏始终铺满窗口：长导出仅右侧滚动，切页保持同一侧栏，窄屏不带动外层", { timeout: 60_000 }, async () => {
	const root = await mkdtemp(join(await testRoot(), "workspace-scroll-"));
	const home = join(root, "home");
	await mkdir(home);
	await configureTestModel(home);
	const pool = new WebSessionPool({
		cwd: root,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	const slot = await pool.create();
	for (let index = 0; index < 30; index++)
		slot.session.sessionManager.appendMessage({
			role: "user",
			content: `第 ${index + 1} 段检查结果。`,
			timestamp: Date.now(),
		});
	const port = await freePort(),
		origin = `http://127.0.0.1:${port}`;
	const api = createWebApi(pool, home, port, undefined, { exportRenderer: { async render() {}, async close() {} } });
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions: [], stage: "workspace-scroll" }),
		api,
	);
	await listenWebServer(server, port);
	let browser: BrowserProbe | undefined;
	try {
		browser = await BrowserProbe.launch(root);
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 1920,
			height: 720,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await browser.navigate(`${origin}/s/${slot.id}/export`);
		await browser.until("!document.querySelector('.export-generate')?.disabled", "导出预览就绪");
		await browser.evaluate("void (window.testSidebar=document.querySelector('.sidebar'))");
		await browser.call("Input.dispatchMouseEvent", { type: "mouseWheel", x: 500, y: 500, deltaY: 600, deltaX: 0 });
		await browser.until(
			"scrollY>0 || document.querySelector('.export-workspace').scrollTop>0",
			"右侧内容已滚动",
			3000,
		);
		const geometry = await browser.evaluate<{
			windowTop: number;
			bodyHeight: number;
			height: number;
			sideTop: number;
			sideBottom: number;
			mainScroll: number;
		}>(`(() => {
			const side=document.querySelector('.sidebar').getBoundingClientRect();
			return {windowTop:scrollY,bodyHeight:document.documentElement.scrollHeight,height:innerHeight,sideTop:side.top,sideBottom:side.bottom,mainScroll:document.querySelector('.export-workspace').scrollTop};
		})()`);
		assert.equal(geometry.windowTop, 0, "外层页面不能随右侧内容滚动");
		assert.equal(geometry.bodyHeight, geometry.height);
		assert.equal(geometry.sideTop, 0);
		assert.equal(geometry.sideBottom, geometry.height);
		assert.ok(geometry.mainScroll > 0);
		for (const path of [`/s/${slot.id}`, `/s/${slot.id}/tree`, "/sessions", "/settings", "/account", "/"]) {
			// 路由链接与历史导航均保留同一个工作台实例。
			const selector =
				path === `/s/${slot.id}`
					? ".export-back"
					: path === "/"
						? ".sidebar-new"
						: path === "/settings"
							? ".sidebar-footer a[href='/settings']"
							: null;
			if (selector && (await browser.evaluate(`!!document.querySelector(${JSON.stringify(selector)})`)))
				await browser.click(selector);
			else
				await browser.evaluate(
					`(() => { window.history.pushState({},'',${JSON.stringify(path)});window.dispatchEvent(new PopStateEvent('popstate')); })()`,
				);
			await browser.until(
				`location.pathname===${JSON.stringify(path)} && !!document.querySelector('.workspace-content > :is(main, section)')`,
				"切换工作区页面",
			);
			assert.equal(await browser.evaluate("window.testSidebar===document.querySelector('.sidebar')"), true);
			assert.equal(
				await browser.evaluate(
					"scrollY===0 && document.documentElement.scrollHeight===innerHeight && document.querySelector('.sidebar').getBoundingClientRect().bottom===innerHeight",
				),
				true,
				path,
			);
		}
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 390,
			height: 720,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await browser.evaluate(
			`window.history.pushState({},'',${JSON.stringify(`/s/${slot.id}/export`)});window.dispatchEvent(new PopStateEvent('popstate'))`,
		);
		await browser.until("!!document.querySelector('.export-workspace')", "窄屏导出页");
		if (await browser.evaluate("!document.querySelector('.app-layout').classList.contains('sidebar-collapsed')"))
			await browser.click('[aria-label="收起侧栏"]');
		await browser.until("!document.querySelector('.sidebar-backdrop')", "窄屏侧栏收起");
		await browser.call("Input.dispatchMouseEvent", { type: "mouseWheel", x: 385, y: 200, deltaY: 600, deltaX: 0 });
		await browser.until("document.querySelector('.export-workspace').scrollTop>0", "窄屏右侧独立滚动");
		assert.equal(await browser.evaluate("scrollY===0 && document.documentElement.scrollWidth<=innerWidth"), true);
	} finally {
		await browser?.close();
		await api.close();
		await pool.close();
		await pool.catalog.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	}
});
