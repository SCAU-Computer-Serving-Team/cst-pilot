import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { existsSync } from "node:fs";
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

const usage = {
	input: 1,
	output: 1,
	cacheRead: 0,
	cacheWrite: 0,
	totalTokens: 2,
	cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
};

test("会话管理：多选、取消、真实删除、运行冲突、断线与自动退出", { timeout: 150_000 }, async () => {
	const root = join(await testRoot(), "session-management");
	await mkdir(root, { recursive: true });
	const home = await mkdtemp(join(root, "home-"));
	let release: (() => void) | undefined;
	const model = createServer(async (request, response) => {
		for await (const _chunk of request) {
			/* 消费请求体后保持真实 Pi 任务运行。 */
		}
		await new Promise<void>((resolve) => {
			release = resolve;
		});
		response.writeHead(200, { "content-type": "text/event-stream" });
		response.end(
			`data: ${JSON.stringify({ id: "hold", choices: [{ index: 0, delta: { content: "完成" }, finish_reason: "stop" }] })}\n\ndata: [DONE]\n\n`,
		);
	});
	await new Promise<void>((resolve) => model.listen(0, "127.0.0.1", resolve));
	const modelAddress = model.address();
	if (!modelAddress || typeof modelAddress === "string") throw new Error("模型端口不可用");
	await writeFile(
		join(home, "settings.json"),
		JSON.stringify({ defaultProvider: "probe", defaultModel: "one", retry: { enabled: false } }),
	);
	await writeFile(
		join(home, "models.json"),
		JSON.stringify({
			providers: {
				probe: {
					api: "openai-completions",
					baseUrl: `http://127.0.0.1:${modelAddress.port}/v1`,
					apiKey: "test-only",
					models: [
						{
							id: "one",
							name: "管理测试模型",
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
		withLoader: (load) => load(),
	});
	async function create(title: string) {
		const slot = await pool.createWithKey(randomUUID());
		slot.session.setSessionName(title);
		slot.session.sessionManager.appendMessage({
			role: "user",
			content: [{ type: "text", text: title }],
			timestamp: Date.now(),
		});
		slot.session.sessionManager.appendMessage({
			role: "assistant",
			content: [{ type: "text", text: "已保存" }],
			api: "openai-completions",
			provider: "probe",
			model: "one",
			usage,
			stopReason: "stop",
			timestamp: Date.now(),
		});
		return slot;
	}
	const current = await create("当前会话");
	const other = await create("待删除会话");
	const keep = await create("保留会话");
	const running = await create("运行会话");
	const attachmentDir = join(home, "web-inbox", "attachments", current.id);
	await mkdir(attachmentDir, { recursive: true });
	await writeFile(join(attachmentDir, "fixture.png"), Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAAB", "base64"));
	const port = await freePort();
	const origin = `http://127.0.0.1:${port}`;
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
		const row = (id: string) => `.sidebar-session[data-session-id="${id}"]`;
		async function rightClick(id: string) {
			const point = await probe.evaluate<{ x: number; y: number }>(
				`(async () => {
					const e = document.querySelector(${JSON.stringify(row(id))});
					e.scrollIntoView({block:'center'});
					let previous, stable = 0;
					for (let frame = 0; frame < 90; frame++) {
						await new Promise(resolve => requestAnimationFrame(resolve));
						const r = e.getBoundingClientRect();
						stable = previous && Math.abs(r.x-previous.x)<.25 && Math.abs(r.y-previous.y)<.25 && Math.abs(r.width-previous.width)<.25 ? stable+1 : 0;
						previous = r;
						const x = r.x+r.width/2, y = r.y+r.height/2;
						if (stable >= 3 && e.contains(document.elementFromPoint(x,y))) return {x,y};
					}
					throw new Error('右键目标仍在移动或被遮挡');
				})()`,
			);
			await probe.call("Input.dispatchMouseEvent", {
				type: "mousePressed",
				button: "right",
				clickCount: 1,
				...point,
			});
			await probe.call("Input.dispatchMouseEvent", {
				type: "mouseReleased",
				button: "right",
				clickCount: 1,
				...point,
			});
		}
		async function pressEscape() {
			await probe.call("Input.dispatchKeyEvent", {
				type: "keyDown",
				key: "Escape",
				code: "Escape",
				windowsVirtualKeyCode: 27,
			});
			await probe.call("Input.dispatchKeyEvent", {
				type: "keyUp",
				key: "Escape",
				code: "Escape",
				windowsVirtualKeyCode: 27,
			});
		}
		async function manage(id: string) {
			await probe.until(
				"!document.querySelector('.app-layout')?.classList.contains('sidebar-collapsed') && Math.abs(document.querySelector('.sidebar')?.getBoundingClientRect().x) < .5 && getComputedStyle(document.querySelector('.sidebar')).visibility !== 'hidden'",
				"侧栏可操作",
			);
			await rightClick(id);
			await probe.click(".session-menu button:has(svg.lucide-list-checks)");
			await probe.until("!!document.querySelector('.sidebar[data-managing]')", "管理模式");
			await probe.until(
				"!document.querySelector('.session-delete-notice[data-state=closing]')",
				"上一条删除结果退场完成",
			);
		}
		async function confirmDelete(id: string) {
			await rightClick(id);
			await probe.click(".session-menu .sidebar-menu-delete");
			await probe.until("!!document.querySelector('.session-delete-dialog[open]')", "删除确认");
			await probe.click(".session-delete-dialog .exit-confirm");
			await probe.until(
				"!document.querySelector('.sidebar[data-managing]') && !!document.querySelector('.session-delete-notice')",
				"操作结束并退出管理",
			);
		}
		async function assertNoticeVisible(label: string) {
			await probe.evaluate(
				"Promise.all([...document.querySelectorAll('.notification')].flatMap(e=>e.getAnimations()).map(a=>a.finished))",
			);
			const geometry = await probe.evaluate<{ left: number; right: number; viewport: number; overflow: boolean }>(
				"(()=>{const notice=document.querySelector('.session-delete-notice'),r=notice.getBoundingClientRect();return {left:r.left,right:r.right,viewport:innerWidth,overflow:notice.scrollWidth>notice.clientWidth}})()",
			);
			assert.ok(geometry.left >= 0 && geometry.right <= geometry.viewport + 1, `${label} 通知可见且不越界`);
			assert.equal(geometry.overflow, false, `${label} 内容不溢出`);
			await probe.screenshot(join(root, `${label}.png`));
		}
		await probe.navigate(`${origin}/s/${current.id}`);
		await probe.until("document.querySelectorAll('.sidebar-session').length===4", "真实会话列表");
		await manage(current.id);
		assert.equal(await probe.evaluate("document.querySelectorAll('.sidebar-session[aria-checked=true]').length"), 1);
		await rightClick(other.id);
		assert.deepEqual(
			await probe.evaluate<string[]>(
				"[...document.querySelectorAll('.session-menu [role=menuitem]')].map(e=>e.textContent.trim())",
			),
			["删除"],
		);
		assert.equal(
			await probe.evaluate("document.querySelectorAll('.sidebar-session[aria-checked=true]').length"),
			1,
			"右键未选项不改变集合",
		);
		await pressEscape();
		assert.equal(
			await probe.evaluate(
				"!!document.querySelector('.sidebar[data-managing]') && !document.querySelector('.session-menu')",
			),
			true,
		);
		await pressEscape();
		await probe.until("!document.querySelector('.sidebar[data-managing]')", "再次Esc退出管理");
		await manage(current.id);
		await probe.click(row(current.id));
		await rightClick(other.id);
		assert.equal(await probe.evaluate("!!document.querySelector('.session-menu')"), false, "零项不提供删除");
		await probe.click(".session-management-toolbar button:first-of-type");
		assert.equal(await probe.evaluate("document.querySelectorAll('.sidebar-session[aria-checked=true]').length"), 4);
		await probe.click(".session-management-toolbar button:first-of-type");
		assert.equal(await probe.evaluate("document.querySelectorAll('.sidebar-session[aria-checked=true]').length"), 0);
		await probe.click(row(current.id));
		await probe.click(row(other.id));
		assert.equal(await probe.evaluate("location.pathname"), `/s/${current.id}`, "选择不导航");
		await rightClick(keep.id);
		await probe.click(".session-menu .sidebar-menu-delete");
		assert.equal(
			await probe.evaluate("document.querySelector('#session-delete-title').textContent"),
			"删除 2 个会话？",
		);
		assert.equal(await probe.evaluate("document.activeElement.textContent"), "取消");
		await pressEscape();
		await probe.until("!document.querySelector('.session-delete-dialog')", "取消确认");
		assert.equal(await probe.evaluate("document.querySelectorAll('.sidebar-session[aria-checked=true]').length"), 2);
		await rightClick(keep.id);
		await probe.screenshot(join(root, "multi-light.png"));
		await probe.evaluate(
			`fetch('/api/settings',{method:'PATCH',headers:{'Content-Type':'application/json','X-CST-Web-Request':'1','Idempotency-Key':crypto.randomUUID()},body:JSON.stringify({theme:'dark'})})`,
		);
		await probe.until("document.documentElement.dataset.theme==='dark'", "深色主题同步");
		await probe.screenshot(join(root, "multi-dark.png"));
		await probe.call("Emulation.setDeviceMetricsOverride", {
			width: 960,
			height: 900,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await rightClick(keep.id);
		await probe.screenshot(join(root, "multi-narrow-dark.png"));
		await probe.click(".session-menu .sidebar-menu-delete");
		await probe.evaluate(
			"window.__deleteCalls=0;window.__originalFetch=window.fetch;window.fetch=async(...args)=>{if(args[1]?.method==='DELETE'){window.__deleteCalls++;if(window.__deleteCalls===1)await new Promise(r=>window.__releaseDelete=r);}return window.__originalFetch(...args)}",
		);
		await probe.click(".session-delete-dialog .exit-confirm");
		await probe.until("!!window.__releaseDelete", "首个删除请求待返回");
		await probe.until(
			"!document.querySelector('.sidebar[data-managing]') && !document.querySelector('.session-delete-dialog') && location.pathname==='/'",
			"请求未返回也立即退出管理",
		);
		assert.equal(await probe.evaluate("document.querySelectorAll('.sidebar-session').length"), 2, "选中项立即隐藏");
		assert.ok(
			(await pool.list()).some((row) => row.id === current.id),
			"服务器尚未删除，界面已乐观更新",
		);
		assert.equal(
			await probe.evaluate("!!document.querySelector('.session-delete-notice')"),
			false,
			"确认前不伪造成功",
		);
		await pressEscape();
		assert.equal(await probe.evaluate("window.__deleteCalls"), 1, "重复操作没有重复请求");
		await probe.screenshot(join(root, "deleting-narrow-dark.png"));
		await probe.evaluate("window.__releaseDelete()");
		await probe.until(
			"!document.querySelector('.sidebar[data-managing]') && document.querySelector('.session-delete-notice')?.textContent.includes('已删除 2 个会话')",
			"成功后退出管理",
		);
		assert.equal(await probe.evaluate("window.__deleteCalls"), 2);
		await probe.evaluate("window.fetch=window.__originalFetch");
		await probe.until(
			"location.pathname==='/' && document.querySelectorAll('a.sidebar-session').length===2",
			"当前会话删除后回新对话",
		);
		assert.equal(existsSync(attachmentDir), false, "真实附件目录已删除");
		assert.ok((await pool.list()).every((item) => ![current.id, other.id].includes(item.id)));
		await assertNoticeVisible("success-narrow-dark");
		await probe.evaluate("document.documentElement.dataset.theme='light'");
		await assertNoticeVisible("success-narrow-light");
		await probe.evaluate("document.documentElement.dataset.theme='dark'");
		await probe.navigate(`${origin}/s/${keep.id}/tree`);
		await probe.until("!!document.querySelector('.tree-row-current')", "分支当前叶就绪");
		await probe.call("Network.enable");
		await probe.call("Network.emulateNetworkConditions", {
			offline: true,
			latency: 0,
			downloadThroughput: 0,
			uploadThroughput: 0,
		});
		await probe.until("!!document.querySelector('.connection-status')", "持续断线通知");
		await probe.evaluate("document.querySelector('.tree-row-current').click()");
		await probe.until("!!document.querySelector('.tree-toast')", "分支消息通知");
		await probe.evaluate(
			"Promise.all([...document.querySelectorAll('.notification')].flatMap(e=>e.getAnimations()).map(a=>a.finished))",
		);
		const stack = await probe.evaluate<{ gap: number; count: number; focus: boolean }>(`(() => {
			const region=document.querySelector(".notification-region"),cards=[...region.children].map(e=>e.getBoundingClientRect());
			return {count:cards.length,gap:cards[1].top-cards[0].bottom,focus:!region.contains(document.activeElement)};
		})()`);
		assert.equal(stack.count, 2);
		assert.ok(stack.gap >= 0, "多条通知不重叠");
		assert.equal(stack.focus, true, "通知不抢焦点");
		await probe.screenshot(join(root, "notifications-stacked-dark.png"));
		await probe.click(".tree-toast .notification-close");
		await probe.until(
			"!document.querySelector('.tree-toast') && !!document.querySelector('.connection-status')",
			"只关闭所选通知",
		);
		await probe.call("Network.emulateNetworkConditions", {
			offline: false,
			latency: 0,
			downloadThroughput: -1,
			uploadThroughput: -1,
		});
		await probe.until("!document.querySelector('.connection-status')", "恢复后撤下持续通知");
		await probe.call("Emulation.setDeviceMetricsOverride", {
			width: 1600,
			height: 1000,
			deviceScaleFactor: 1,
			mobile: false,
		});
		const partial = await create("部分成功会话");
		await probe.navigate(`${origin}/s/${running.id}`);
		await probe.fill(".composer-input", "HOLD_SESSION_MANAGEMENT");
		await probe.click('[aria-label="发送消息"]');
		await probe.until("!!document.querySelector('[aria-label=停止生成]')", "真实任务正在执行");
		await probe.click(row(keep.id));
		await probe.until(
			`location.pathname==='/s/${keep.id}' && document.querySelector('.chat-header h1')?.textContent==='保留会话' && !document.querySelector('[data-home-transition]')`,
			"切换保留会话完成",
		);
		await manage(running.id);
		await probe.click(row(partial.id));
		await confirmDelete(keep.id);
		assert.match(
			await probe.evaluate<string>("document.querySelector('.session-delete-notice').textContent"),
			/已删除 1 个，1 个失败/,
		);
		assert.equal(await probe.evaluate("location.pathname"), `/s/${keep.id}`);
		assert.ok(pool.get(running.id)?.session.isStreaming, "失败不能停止运行中的会话");
		assert.ok((await pool.list()).some((item) => item.id === running.id));
		assert.ok((await pool.list()).every((item) => item.id !== partial.id));
		assert.equal(await probe.evaluate("document.querySelectorAll('.sidebar-session[role=checkbox]').length"), 0);
		await assertNoticeVisible("partial-dark");
		await manage(running.id);
		await confirmDelete(keep.id);
		assert.match(
			await probe.evaluate<string>("document.querySelector('.session-delete-notice').textContent"),
			/删除失败.*正在运行/s,
		);
		assert.ok(pool.get(running.id)?.session.isStreaming);
		await probe.screenshot(join(root, "failure-dark.png"));
		await manage(keep.id);
		await rightClick(keep.id);
		await probe.click(".session-menu .sidebar-menu-delete");
		await probe.evaluate(
			"window.__originalFetch=window.fetch;window.__failedDeletes=0;window.fetch=(...args)=>{if(args[1]?.method==='DELETE'){window.__failedDeletes++;return Promise.reject(new TypeError('test offline'))}return window.__originalFetch(...args)}",
		);
		await probe.click(".session-delete-dialog .exit-confirm");
		await probe.until(
			"!document.querySelector('.sidebar[data-managing]') && !!document.querySelector('.session-delete-notice[data-kind=danger]')",
			"断线也退出管理",
		);
		assert.equal(await probe.evaluate("window.__failedDeletes"), 1);
		assert.ok((await pool.list()).some((item) => item.id === keep.id));
		await probe.until(
			`location.pathname==='/s/${keep.id}' && !!document.querySelector(${JSON.stringify(row(keep.id))})`,
			"当前会话删除失败恢复原会话",
		);
		await probe.evaluate("window.fetch=window.__originalFetch");
		await probe.click(".session-delete-close");
		await probe.until("!document.querySelector('.session-delete-notice')", "结果可关闭");
		await manage(keep.id);
		await probe.click(".session-management-toolbar button:last-of-type");
		assert.equal(
			await probe.evaluate("document.querySelectorAll('.sidebar-session[role=checkbox]').length"),
			0,
			"完成清除选择",
		);
		await manage(keep.id);
		await probe.click(".sidebar-icon");
		await probe.until("!document.querySelector('.sidebar[data-managing]')", "收起侧栏取消管理");
		await probe.click(".sidebar-restore");
		await probe.until("!document.querySelector('.sidebar-collapsed')", "恢复侧栏");
		const single = await create("单项删除会话");
		await probe.navigate(`${origin}/s/${keep.id}`);
		await probe.until(
			`!!document.querySelector(${JSON.stringify(row(single.id))}) && document.querySelector('.chat-header h1')?.textContent==='保留会话'`,
			"回滚测试准备",
		);
		await manage(keep.id);
		await probe.click(row(running.id));
		await rightClick(keep.id);
		await probe.click(".session-menu .sidebar-menu-delete");
		await probe.evaluate(
			"window.__originalFetch=window.fetch;window.__failedDeletes=0;window.fetch=async(...args)=>{if(args[1]?.method==='DELETE'){window.__failedDeletes++;if(window.__failedDeletes===1)await new Promise(r=>window.__releaseFailures=r);throw new TypeError('test offline')}return window.__originalFetch(...args)}",
		);
		await probe.click(".session-delete-dialog .exit-confirm");
		await probe.until(
			"location.pathname==='/' && !document.querySelector('.sidebar[data-managing]') && !!window.__releaseFailures",
			"失败请求待返回，已乐观退出",
		);
		await probe.click(row(single.id));
		await probe.until(`location.pathname==='/s/${single.id}'`, "删除期间另行导航");
		await probe.evaluate("window.__releaseFailures()");
		await probe.until(
			"!document.querySelector('.sidebar[data-managing]') && !!document.querySelector('.session-delete-notice details')",
			"多项失败退出管理",
		);
		assert.equal(await probe.evaluate("window.__failedDeletes"), 2);
		assert.equal(await probe.evaluate("location.pathname"), `/s/${single.id}`, "回滚不覆盖后续导航");
		assert.equal(
			await probe.evaluate("document.querySelector('.session-delete-notice details').open"),
			false,
			"多项原因默认折叠",
		);
		await probe.click(".session-delete-notice summary");
		assert.equal(await probe.evaluate("document.querySelectorAll('.session-delete-notice li').length"), 2);
		await probe.evaluate("window.fetch=window.__originalFetch");
		await assertNoticeVisible("multiple-failures-dark");
		await probe.navigate(`${origin}/s/${keep.id}`);
		await probe.until(`!!document.querySelector(${JSON.stringify(row(single.id))})`, "新增单项会话");
		await rightClick(single.id);
		assert.deepEqual(
			await probe.evaluate<string[]>(
				"[...document.querySelectorAll('.session-menu [role=menuitem]')].map(e=>e.textContent.trim())",
			),
			["重命名", "管理", "删除"],
		);
		await probe.call("Input.dispatchKeyEvent", { type: "keyDown", key: "End", code: "End" });
		assert.equal(await probe.evaluate("document.activeElement.textContent.trim()"), "删除", "菜单End聚焦删除");
		await probe.call("Input.dispatchKeyEvent", {
			type: "keyDown",
			key: "Enter",
			code: "Enter",
			text: "\r",
			windowsVirtualKeyCode: 13,
		});
		await probe.call("Input.dispatchKeyEvent", {
			type: "keyUp",
			key: "Enter",
			code: "Enter",
			windowsVirtualKeyCode: 13,
		});
		await probe.until("!!document.querySelector('.session-delete-dialog[open]')", "单会话删除确认");
		assert.equal(
			await probe.evaluate("document.querySelector('#session-delete-title').textContent"),
			"删除 1 个会话？",
		);
		await probe.click(".session-delete-dialog .exit-confirm");
		await probe.until(
			"!document.querySelector('.session-delete-dialog') && document.querySelector('.session-delete-notice')?.textContent.includes('已删除 1 个会话')",
			"普通单会话删除",
		);
		await assertNoticeVisible("single-deleted-dark");
		assert.ok((await pool.list()).every((item) => item.id !== single.id));
		const aborted = await fetch(`${origin}/api/sessions/${running.id}/abort`, {
			method: "POST",
			headers: {
				"Content-Type": "application/json",
				Origin: origin,
				"X-CST-Web-Request": "1",
				"Idempotency-Key": randomUUID(),
			},
			body: "{}",
		});
		assert.equal(aborted.status, 200);
		assert.equal(pool.get(running.id)?.session.isStreaming, false);
		const last = await create("最后会话");
		await probe.navigate(`${origin}/s/${keep.id}`);
		await probe.until(
			`!!document.querySelector(${JSON.stringify(row(last.id))}) && !document.querySelector('[data-home-transition]')`,
			"终止任务后列表稳定",
		);
		await manage(keep.id);
		await probe.click(".session-management-toolbar button:first-of-type");
		await rightClick(keep.id);
		await probe.click(".session-menu .sidebar-menu-delete");
		await probe.evaluate(
			"window.__originalFetch=window.fetch;window.fetch=(...args)=>{if(String(args[0])==='/api/state'){return new Promise(resolve=>window.__releaseList=()=>resolve(window.__originalFetch(...args)))}return window.__originalFetch(...args)}",
		);
		await probe.click(".session-delete-dialog .exit-confirm");
		await probe.until(
			"!document.querySelector('.sidebar[data-managing]') && !document.querySelector('.session-delete-dialog') && document.querySelector('.session-delete-notice')?.textContent.includes('已删除 3 个会话')",
			"列表重取未完成也退出管理",
		);
		await probe.until(
			"location.pathname==='/' && !document.querySelector('.sidebar-session') && !!document.querySelector('.sidebar-new')",
			"全部删除后保留新对话",
		);
		assert.deepEqual(await pool.list(), []);
		await probe.evaluate("window.fetch=window.__originalFetch;window.__releaseList?.()");
		await probe.screenshot(join(root, "all-deleted-dark.png"));
		await probe.navigate(`${origin}/`);
		await probe.until(
			"!document.querySelector('.sidebar-session') && !document.querySelector('.sidebar[data-managing]')",
			"刷新保持空列表且不恢复管理",
		);
		await writeFile(
			join(root, "report.json"),
			JSON.stringify(
				{
					success: true,
					covered: [
						"selection",
						"empty",
						"all",
						"escape",
						"cancel",
						"light",
						"dark",
						"narrow",
						"busy",
						"duplicate",
						"current-session",
						"attachments",
						"partial",
						"running-conflict",
						"offline",
						"finish",
						"collapse",
						"multiple-failures",
						"keyboard-menu",
						"single-delete",
						"delayed-list-refresh",
						"all-deleted",
						"refresh",
					],
				},
				null,
				2,
			),
		);
	} finally {
		release?.();
		await browser?.close();
		await api.close();
		await pool.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		model.closeAllConnections();
		await new Promise<void>((resolve) => model.close(() => resolve()));
		await rm(home, { recursive: true, force: true });
	}
});
