import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

test("错误状态：真实401、持续断线、恢复不重发与运行任务退出确认", { timeout: 60000 }, async () => {
	const root = join(await testRoot(), "error-states");
	await mkdir(root, { recursive: true });
	const home = await mkdtemp(join(root, "home-"));
	let calls = 0;
	let release: (() => void) | undefined;
	const model = createServer(async (req, res) => {
		let raw = "";
		for await (const c of req) raw += c;
		calls++;
		if (raw.includes("HOLD_EXIT")) {
			await new Promise<void>((r) => {
				release = r;
			});
			res.end();
			return;
		}
		if (raw.includes("REQUEST_FAIL")) {
			res.writeHead(500, { "content-type": "application/json" });
			res.end('{"error":{"message":"REQUEST_FAILED_FOR_TEST"}}');
			return;
		}
		res.writeHead(401, { "content-type": "application/json" });
		res.end('{"error":{"type":"authentication_error","message":"EXPIRED_FOR_TEST"}}');
	});
	await new Promise<void>((r) => model.listen(0, "127.0.0.1", r));
	const address = model.address();
	if (!address || typeof address === "string") throw Error("模型端口");
	await writeFile(
		join(home, "settings.json"),
		JSON.stringify({
			theme: "light",
			defaultProvider: "probe",
			defaultModel: "one",
			retry: { enabled: false, maxRetries: 0 },
		}),
	);
	await writeFile(
		join(home, "models.json"),
		JSON.stringify({
			providers: {
				probe: {
					api: "openai-completions",
					baseUrl: `http://127.0.0.1:${address.port}/v1`,
					apiKey: "not-secret",
					models: [
						{
							id: "one",
							name: "错误测试模型",
							input: ["text"],
							reasoning: false,
							contextWindow: 10000,
							maxTokens: 100,
						},
					],
				},
			},
		}),
	);
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (f) => f(),
	});
	const port = await freePort(),
		origin = `http://127.0.0.1:${port}`;
	let exits = 0;
	const api = createWebApi(pool, home, port, () => exits++);
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
		async function assertSurface(selector: string, label: string) {
			for (const width of [1600, 960]) {
				await probe.call("Emulation.setDeviceMetricsOverride", {
					width,
					height: 1000,
					deviceScaleFactor: 1,
					mobile: false,
				});
				if (width <= 960 && !(await probe.evaluate("!!document.querySelector('.exit-dialog')"))) {
					await probe.click('button[aria-label="收起侧栏"]');
					await probe.until(
						"document.querySelector('.sidebar').getBoundingClientRect().right<=.5",
						"窄屏内容不受侧栏遮挡",
					);
				}
				for (const theme of ["light", "dark"]) {
					await probe.evaluate(`document.documentElement.dataset.theme=${JSON.stringify(theme)}`);
					await probe.evaluate("document.fonts.ready");
					const surface = await probe.evaluate<{ left: number; right: number; overflow: boolean }>(`(() => {
						const e=document.querySelector(${JSON.stringify(selector)}); e.scrollIntoView({block:"center"});
						const r=e.getBoundingClientRect();
						return {left:r.left,right:r.right,overflow:e.scrollWidth>e.clientWidth};
					})()`);
					assert.ok(surface.left >= 0 && surface.right <= width + 1, `${label} stays inside viewport`);
					assert.equal(surface.overflow, false, `${label} no horizontal overflow`);
					const name = `${label}-${width}-${theme}`;
					await writeFile(join(root, `${name}.json`), JSON.stringify(surface, null, 2));
					await probe.screenshot(join(root, `${name}.png`));
				}
			}
			await probe.call("Emulation.setDeviceMetricsOverride", {
				width: 1600,
				height: 1000,
				deviceScaleFactor: 1,
				mobile: false,
			});
			await probe.evaluate("document.documentElement.dataset.theme='light'");
			if (!(await probe.evaluate("!!document.querySelector('.exit-dialog')"))) {
				await probe.click('button[aria-label="展开侧栏"]');
				await probe.until(
					"Math.abs(document.querySelector('.sidebar').getBoundingClientRect().x)<.5",
					"恢复桌面侧栏",
				);
			}
		}
		await browser.navigate(`${origin}/`);
		await browser.fill(".composer-input", "AUTH_FAIL");
		await browser.click('[aria-label="发送消息"]');
		await browser.until("!!document.querySelector('.error-notice[data-kind=auth]')", "401提供重新登录");
		assert.equal(await browser.evaluate("document.querySelector('.error-details').open"), false);
		assert.equal(await browser.evaluate("document.querySelector('.error-notice strong').textContent"), "登录已失效");
		assert.equal(await browser.evaluate("document.querySelector('.error-notice p')"), null);
		assert.ok(
			!(await browser.evaluate<string>("document.querySelector('.error-notice').innerText")).includes(
				"EXPIRED_FOR_TEST",
			),
		);
		await assertSurface(".error-notice", "auth");
		await browser.click(".error-details summary");
		assert.equal(await browser.evaluate("document.querySelector('.error-details').open"), true);
		assert.match(
			await browser.evaluate<string>("document.querySelector('.error-details pre').textContent"),
			/EXPIRED_FOR_TEST/,
		);
		await assertSurface(".error-details", "details");
		await browser.evaluate("document.querySelector('.error-details summary').focus()");
		for (const type of ["keyDown", "keyUp"]) {
			await browser.call("Input.dispatchKeyEvent", {
				type,
				key: "Enter",
				code: "Enter",
				...(type === "keyDown" ? { text: "\r" } : {}),
				windowsVirtualKeyCode: 13,
			});
		}
		await browser.until("!document.querySelector('.error-details').open", "键盘收起技术详情");
		await browser.fill(".composer-input", "REQUEST_FAIL");
		await browser.click('[aria-label="发送消息"]');
		await browser.until("!!document.querySelector('.error-notice[data-kind=general]')", "模型失败状态");
		assert.equal(
			await browser.evaluate("document.querySelector('.error-notice[data-kind=general] strong').textContent"),
			"请求失败",
		);
		assert.equal(
			await browser.evaluate("document.querySelector('.error-notice[data-kind=general] p').textContent"),
			"消息已保存。",
		);
		await browser.call("Network.enable");
		await browser.call("Network.emulateNetworkConditions", {
			offline: true,
			latency: 0,
			downloadThroughput: 0,
			uploadThroughput: 0,
		});
		await browser.until("!!document.querySelector('.connection-status')", "断线持续bar");
		await browser.evaluate(
			"Promise.all(document.querySelector('.connection-status').getAnimations().map(a=>a.finished))",
		);
		const connectionGeometry = await browser.evaluate<{
			left: number;
			right: number;
			top: number;
			viewport: number;
			contentLeft: number;
		}>(
			"(()=>{const r=document.querySelector('.connection-status').getBoundingClientRect();return {left:r.left,right:r.right,top:r.top,viewport:innerWidth,contentLeft:document.querySelector('.sidebar').inert?0:document.querySelector('.sidebar').getBoundingClientRect().width};})()",
		);
		assert.ok(
			Math.abs(
				(connectionGeometry.left + connectionGeometry.right) / 2 -
					(connectionGeometry.viewport + connectionGeometry.contentLeft) / 2,
			) < 1,
			"断线提示在主内容区域居中",
		);
		assert.equal(connectionGeometry.top, 20, "断线提示位于顶部20px");
		assert.equal(
			await browser.evaluate("document.querySelector('.connection-status strong').textContent"),
			"连接断开",
		);
		assert.equal(
			await browser.evaluate("document.querySelector('.connection-status p').textContent"),
			"正在重连，输入已保留。",
		);
		await browser.screenshot(join(root, "offline-light.png"));
		const count = calls;
		await browser.call("Network.emulateNetworkConditions", {
			offline: false,
			latency: 0,
			downloadThroughput: -1,
			uploadThroughput: -1,
		});
		await browser.until("!document.querySelector('.connection-status')", "恢复撤下提示");
		assert.equal(calls, count, "恢复不能重发已接受消息");
		await browser.until("!document.querySelector('[data-home-transition]')", "首轮页面过渡清理");
		await browser.click(".sidebar-account-trigger");
		await browser.click(".sidebar-account-item:has(svg.lucide-power)");
		await browser.until(
			"document.querySelector('.exit-dialog > p')?.textContent==='将关闭服务和终端。'",
			"空闲退出确认",
		);
		assert.equal(await browser.evaluate("document.querySelector('.exit-description').textContent"), "会话保留。");
		await assertSurface(".exit-dialog", "exit-idle");
		await browser.click(".exit-actions button:not(.exit-confirm)");
		assert.equal(exits, 0);
		await browser.fill(".composer-input", "HOLD_EXIT");
		await browser.click('[aria-label="发送消息"]');
		await browser.until("!!document.querySelector('[aria-label=停止生成]')", "运行状态");
		await browser.click(".sidebar-account-trigger");
		await browser.click(".sidebar-account-item:has(svg.lucide-power)");
		await browser.until("!!document.querySelector('.exit-dialog[open] button:not(:disabled)')", "退出确认弹窗");
		await browser.until(
			"document.querySelector('.exit-dialog').innerText.includes('1 个会话')",
			"退出列出真实运行数",
		);
		assert.equal(await browser.evaluate("document.activeElement.textContent"), "继续使用");
		await browser.evaluate("Promise.all(document.querySelector('.exit-dialog').getAnimations().map(a=>a.finished))");
		const geometry = await browser.evaluate<{
			x: number;
			y: number;
			width: number;
			height: number;
			viewportWidth: number;
			viewportHeight: number;
		}>(
			"(()=>{const r=document.querySelector('.exit-dialog').getBoundingClientRect();return {x:r.x,y:r.y,width:r.width,height:r.height,viewportWidth:innerWidth,viewportHeight:innerHeight};})()",
		);
		assert.ok(
			Math.abs(geometry.x + geometry.width / 2 - geometry.viewportWidth / 2) < 1 &&
				Math.abs(geometry.y + geometry.height / 2 - geometry.viewportHeight / 2) < 1,
			"退出弹窗在视口居中",
		);
		assert.equal(
			await browser.evaluate("document.querySelector('.exit-dialog > p').textContent"),
			"1 个会话运行中；退出将停止任务、暂停队列。",
		);
		assert.equal(
			await browser.evaluate("document.querySelector('.exit-description').textContent"),
			"记录保留，任务不自动重放。",
		);
		await assertSurface(".exit-dialog", "exit-running");
		await browser.call("Input.dispatchKeyEvent", {
			type: "keyDown",
			key: "Escape",
			code: "Escape",
			windowsVirtualKeyCode: 27,
		});
		await browser.call("Input.dispatchKeyEvent", {
			type: "keyUp",
			key: "Escape",
			code: "Escape",
			windowsVirtualKeyCode: 27,
		});
		await browser.until("!document.querySelector('.exit-dialog')", "Esc取消退出");
		assert.equal(exits, 0);
		assert.ok(
			pool.snapshot().some((s) => s.running),
			"取消不停止任务",
		);
		await browser.click(".sidebar-account-trigger");
		await browser.click(".sidebar-account-item:has(svg.lucide-power)");
		await browser.click(".exit-confirm");
		await browser.until(
			"document.querySelector('#exit-title')?.textContent==='CST Pilot 已退出'",
			"停止任务保存后显示退出完成",
		);
		await new Promise((r) => setTimeout(r, 180));
		assert.equal(exits, 1);
		assert.deepEqual(pool.snapshot(), []);
		assert.equal(
			await browser.evaluate("document.querySelector('.exit-description').textContent"),
			"会话已保存。重开请运行 pi.cmd。",
		);
		await assertSurface(".exit-dialog", "exited");
	} finally {
		release?.();
		await browser?.close();
		await api.close();
		await pool.close();
		server.closeAllConnections();
		await new Promise<void>((r) => server.close(() => r()));
		model.closeAllConnections();
		await new Promise<void>((r) => model.close(() => r()));
		await rm(home, { recursive: true, force: true });
	}
});
