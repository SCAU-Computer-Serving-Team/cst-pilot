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

test("管理原位进退、日期整组勾选、一个月内与减少动态", { timeout: 60000 }, async () => {
	const root = join(await testRoot(), "session-group-motion");
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
		await probe.navigate(`http://127.0.0.1:${port}/`);
		await probe.until("document.querySelectorAll('.sidebar-session').length===6", "分组列表");
		assert.deepEqual(
			await probe.evaluate<string[]>(
				"[...document.querySelectorAll('.sidebar-group h2')].map(e=>e.textContent.trim())",
			),
			["今天", "昨天", "一周内", "一个月内", "更早"],
		);
		await probe.evaluate(
			`window.__firstRow=document.querySelector('[data-session-id="${fixtures[0].id}"]');window.__firstCheck=window.__firstRow.querySelector('.session-check-control');window.__baseline=window.__firstRow.getBoundingClientRect().top;window.__textX=window.__firstRow.querySelector('.sidebar-selection-text').getBoundingClientRect().x;window.__groupY=document.querySelector('.sidebar-group h2').getBoundingClientRect().y`,
		);
		await probe.holdAnimations(".session-check-control, .sidebar-selection-text, .session-management-toolbar");
		async function enter() {
			const point = await probe.evaluate<{ x: number; y: number }>(
				`(()=>{const r=window.__firstRow.getBoundingClientRect();return {x:r.x+120,y:r.y+20}})()`,
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
			await probe.click(".session-menu button:has(svg.lucide-list-checks)");
			await probe.until("!!document.querySelector('.sidebar[data-managing]')", "管理展开");
		}
		await enter();
		await probe.until("window.__heldAnimations.length>=3", "进场CSS过渡");
		const duration = await probe.evaluate<number>(
			"window.__heldAnimations.find(a=>a.effect.target===window.__firstCheck && a.transitionProperty==='opacity').effect.getTiming().duration",
		);
		assert.equal(duration, 250);
		await probe.evaluate("window.__heldAnimations.forEach(a=>a.currentTime=40)");
		const intermediate = await probe.evaluate<{ opacity: number; x: number; rowY: number; groupY: number }>(
			"({opacity:+getComputedStyle(window.__firstCheck).opacity,x:window.__firstRow.querySelector('.sidebar-selection-text').getBoundingClientRect().x-window.__textX,rowY:window.__firstRow.getBoundingClientRect().top-window.__baseline,groupY:document.querySelector('.sidebar-group h2').getBoundingClientRect().y-window.__groupY})",
		);
		assert.ok(intermediate.opacity > 0 && intermediate.opacity < 1);
		assert.ok(intermediate.x > 0 && intermediate.x < 24);
		assert.ok(Math.abs(intermediate.rowY) < 0.5 && Math.abs(intermediate.groupY) < 0.5);
		await probe.screenshot(join(root, "enter-half-light.png"));
		await probe.releaseAnimations();
		assert.equal(await probe.evaluate("getComputedStyle(window.__firstCheck).opacity"), "1");
		const checkboxAlignment = await probe.evaluate<{ row: number; group: number; square: boolean }>(`(() => {
			const check=window.__firstCheck.getBoundingClientRect(),text=window.__firstRow.querySelector('.sidebar-selection-text').getBoundingClientRect();
			const group=document.querySelector('.sidebar-group h2'),groupCheck=group.querySelector('.session-check-control').getBoundingClientRect(),groupText=group.querySelector('.sidebar-selection-text').getBoundingClientRect();
			return {row:Math.abs(check.y+check.height/2-text.y-text.height/2),group:Math.abs(groupCheck.y+groupCheck.height/2-groupText.y-groupText.height/2),square:check.width===16&&check.height===16};
		})()`);
		assert.ok(checkboxAlignment.row < 0.5, `会话勾选框与文字中心对齐：${JSON.stringify(checkboxAlignment)}`);
		assert.ok(checkboxAlignment.group < 0.5, `日期勾选框与文字中心对齐：${JSON.stringify(checkboxAlignment)}`);
		assert.equal(checkboxAlignment.square, true);
		const alignment = await probe.evaluate<number>(
			"(()=>{const a=document.querySelector('.sidebar-new').getBoundingClientRect(),b=document.querySelector('.session-management-toolbar').getBoundingClientRect();return Math.abs(a.y+a.height/2-b.y-b.height/2)})()",
		);
		assert.ok(alignment < 0.5, "管理操作与新对话同高");
		assert.equal(
			await probe.evaluate("document.querySelector('.session-management-toolbar').textContent.includes('已选')"),
			false,
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
		assert.equal(await probe.evaluate("document.querySelectorAll('.sidebar-session[aria-checked=true]').length"), 1);
		await probe.screenshot(join(root, "group-selected-light.png"));
		await probe.holdAnimations(".session-check-control, .sidebar-selection-text, .session-management-toolbar");
		await probe.click(".session-management-toolbar button:last-of-type");
		await probe.until(
			"!document.querySelector('.sidebar[data-managing]') && window.__heldAnimations.length>=3",
			"退出CSS过渡",
		);
		assert.equal(
			await probe.evaluate(`document.querySelector('[data-session-id="${fixtures[0].id}"]')===window.__firstRow`),
			true,
		);
		await probe.evaluate("window.__heldAnimations.forEach(a=>a.currentTime=40)");
		const outgoing = await probe.evaluate<number>("+getComputedStyle(window.__firstCheck).opacity");
		assert.ok(outgoing > 0 && outgoing < 1);
		assert.ok(await probe.evaluate("Math.abs(window.__firstRow.getBoundingClientRect().top-window.__baseline)<.5"));
		await probe.screenshot(join(root, "exit-half-light.png"));
		await probe.releaseAnimations();
		assert.equal(await probe.evaluate("getComputedStyle(window.__firstCheck).opacity"), "0");
		await enter();
		await probe.until("getComputedStyle(window.__firstCheck).opacity==='1'", "再次完整进入");
		await probe.holdAnimations(".session-check-control, .sidebar-selection-text, .session-management-toolbar");
		await probe.click(".session-management-toolbar button:last-of-type");
		await probe.until(
			"window.__heldAnimations.some(a=>a.effect.target===window.__firstCheck && a.transitionProperty==='opacity')",
			"捕获反向退出",
		);
		await probe.evaluate("window.__heldAnimations.forEach(a=>a.currentTime=80)");
		const reverseStart = await probe.evaluate<number>("+getComputedStyle(window.__firstCheck).opacity");
		await enter();
		assert.ok(
			await probe.evaluate(
				`+getComputedStyle(window.__firstCheck).opacity<.99 && Math.abs(+getComputedStyle(window.__firstCheck).opacity-${reverseStart})<.2`,
			),
			"中途反向保留当前呈现值",
		);
		await probe.releaseAnimations();
		await probe.click(".session-management-toolbar button:last-of-type");
		await probe.until("getComputedStyle(window.__firstCheck).opacity==='0'", "反向测试结束");
		await probe.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "reduce" }],
		});
		await enter();
		assert.equal(await probe.evaluate("getComputedStyle(window.__firstCheck).transitionDuration"), "0s");
		assert.equal(
			await probe.evaluate(
				"getComputedStyle(window.__firstRow.querySelector('.sidebar-selection-text')).transitionDuration",
			),
			"0s",
		);
		await probe.click(".session-management-toolbar button:last-of-type");
		assert.equal(await probe.evaluate("getComputedStyle(window.__firstCheck).opacity"), "0");
		await writeFile(
			join(root, "report.json"),
			JSON.stringify({ success: true, duration, intermediate, outgoing, dateGroups: 5 }, null, 2),
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
