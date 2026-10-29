import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm, utimes, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

const usage = {
	input: 0,
	output: 0,
	cacheRead: 0,
	cacheWrite: 0,
	totalTokens: 0,
	cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
};

test("会话日期分组、整组勾选、管理可访问性与减少动态", { timeout: 60000 }, async () => {
	const root = join(await testRoot(), "session-groups");
	await mkdir(root, { recursive: true });
	const home = await mkdtemp(join(root, "home-"));
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	const fixtures: { id: string; title: string; days: number }[] = [];
	for (const days of [0, 0, 1, 5, 15, 45]) {
		const slot = await pool.create();
		const date = new Date();
		date.setDate(date.getDate() - days);
		date.setHours(12, 0, 0, 0);
		const title = `分组 ${days} 天 ${fixtures.length}`;
		slot.session.sessionManager.appendMessage({
			role: "user",
			content: [{ type: "text", text: title }],
			timestamp: date.getTime(),
		});
		slot.session.sessionManager.appendMessage({
			role: "assistant",
			content: [{ type: "text", text: "已保存" }],
			api: "openai-completions",
			provider: "probe",
			model: "one",
			usage,
			stopReason: "stop",
			timestamp: date.getTime(),
		});
		slot.session.setSessionName(title);
		assert.ok(slot.session.sessionFile);
		await utimes(slot.session.sessionFile, date, date);
		fixtures.push({ id: slot.id, title, days });
	}
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
		await probe.call("Page.addScriptToEvaluateOnNewDocument", {
			source: `(() => {
				if (location.hash !== '#cst-web-start') return;
				const sample = () => {
					const row = document.querySelector('.sidebar-group h2');
					if (row) window.__firstSessionFrame = { animated: !!row.closest('[data-startup-rows]'), opacity: getComputedStyle(row).opacity };
					else requestAnimationFrame(sample);
				};
				requestAnimationFrame(sample);
			})()`,
		});
		await probe.call("Page.navigate", { url: `http://127.0.0.1:${port}/#cst-web-start` });
		await probe.until("!!document.querySelector('.sidebar[data-startup-rows] .sidebar-group h2')", "启动会话入场");
		assert.equal(await probe.evaluate("location.hash"), "", "启动标记消费后从地址移除");
		await probe.until("!!window.__firstSessionFrame", "会话列表首帧");
		assert.equal(await probe.evaluate("window.__firstSessionFrame.animated"), true, "列表首帧已进入入场状态");
		assert.deepEqual(
			await probe.evaluate<number[]>(
				"[...document.querySelectorAll('.sidebar-group h2, .sidebar-session-row')].map(e=>Number(e.style.getPropertyValue('--startup-row')))",
			),
			[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
			"日期和会话按近到远依次入场",
		);
		await probe.until("!document.querySelector('.sidebar[data-startup-rows]')", "启动入场结束");
		await probe.navigate(`http://127.0.0.1:${port}/`);
		assert.equal(
			await probe.evaluate("!!document.querySelector('[data-home-startup]')"),
			false,
			"普通打开不播放开场",
		);
		await probe.until("document.querySelectorAll('.sidebar-session').length===6", "分组列表");
		assert.equal(
			await probe.evaluate(
				"[...document.querySelectorAll('.sidebar-group-select')].every(e=>e.getAttribute('aria-hidden')==='true' && e.tabIndex===-1)",
			),
			true,
			"普通模式不向辅助技术暴露隐藏勾选框",
		);
		assert.deepEqual(
			await probe.evaluate<string[]>(
				"[...document.querySelectorAll('.sidebar-group h2')].map(e=>e.textContent.trim())",
			),
			["今天", "昨天", "一周内", "一个月内", "更早"],
		);
		async function enter() {
			const point = await probe.evaluate<{ x: number; y: number }>(
				`(()=>{const r=document.querySelector('[data-session-id="${fixtures[0].id}"]').getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2}})()`,
			);
			for (const type of ["mousePressed", "mouseReleased"]) {
				await probe.call("Input.dispatchMouseEvent", { type, button: "right", clickCount: 1, ...point });
			}
			await probe.click(".session-menu button:has(svg.lucide-list-checks)");
			await probe.until("!!document.querySelector('.sidebar[data-managing]')", "进入管理");
		}
		await enter();
		assert.equal(
			await probe.evaluate(
				"[...document.querySelectorAll('.sidebar-group-select')].every(e=>e.getAttribute('aria-hidden')==='false' && e.tabIndex===0)",
			),
			true,
			"管理模式恢复日期组勾选框的可访问性",
		);
		assert.equal(
			await probe.evaluate("document.querySelector('[aria-label=选择今天的会话]').getAttribute('aria-checked')"),
			"mixed",
		);
		await probe.click('[aria-label="选择今天的会话"]');
		assert.equal(
			await probe.evaluate(
				"document.querySelectorAll('.sidebar-group[aria-label=今天] .sidebar-session[aria-checked=true]').length",
			),
			2,
		);
		await probe.click('[aria-label="选择今天的会话"]');
		assert.equal(await probe.evaluate("document.querySelectorAll('.sidebar-session[aria-checked=true]').length"), 0);
		await probe.click('[aria-label="选择一个月内的会话"]');
		assert.equal(
			await probe.evaluate(
				"document.querySelectorAll('.sidebar-group[aria-label=一个月内] .sidebar-session[aria-checked=true]').length",
			),
			1,
		);
		assert.equal(
			await probe.evaluate("document.querySelectorAll('.sidebar-session[aria-checked=true]').length"),
			1,
			"日期组互不串选",
		);
		await probe.screenshot(join(root, "group-selected-light.png"));
		await probe.click(".session-management-toolbar button:last-of-type");
		await probe.until("!document.querySelector('.sidebar[data-managing]')", "退出管理");
		assert.equal(
			await probe.evaluate("document.querySelectorAll('.sidebar-session[aria-checked=true]').length"),
			0,
			"退出管理清除选择",
		);
		await probe.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "reduce" }],
		});
		await probe.call("Page.navigate", { url: "about:blank" });
		await probe.until("location.href === 'about:blank'", "关闭原页面");
		await probe.call("Page.navigate", { url: `http://127.0.0.1:${port}/#cst-web-start` });
		await probe.until(
			"!!document.querySelector('.sidebar[data-startup-rows] .sidebar-group h2')",
			"减少动态下的启动列表",
		);
		assert.equal(
			await probe.evaluate("getComputedStyle(document.querySelector('.sidebar-group h2')).animationName"),
			"none",
			"减少动态时不播放启动滑入",
		);
		await enter();
		await probe.click('[aria-label="选择今天的会话"]');
		await probe.click(".session-management-toolbar button:last-of-type");
		await probe.until("!document.querySelector('.sidebar[data-managing]')", "减少动态下仍可完成管理操作");
		await writeFile(
			join(root, "report.json"),
			JSON.stringify(
				{ success: true, groupSelection: true, accessibleManagement: true, reducedMotion: true },
				null,
				2,
			),
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
