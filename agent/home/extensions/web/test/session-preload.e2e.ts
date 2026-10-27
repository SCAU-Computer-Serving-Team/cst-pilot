import assert from "node:assert/strict";
import { mkdtemp, writeFile } from "node:fs/promises";
import type { ServerResponse } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

test("启动预载近七天最新十项，慢请求下缓存秒切，运行状态扫光及减少动态", { timeout: 45000 }, async () => {
	const root = await mkdtemp(join(await testRoot(), "session-preload-"));
	const now = Date.now();
	const rows = Array.from({ length: 13 }, (_, i) => ({
		id: `s${i}`,
		title: `会话 ${i}`,
		updatedAt: new Date(now - i * 3600000).toISOString(),
		running: i === 0,
	}));
	rows.push({ id: "old", title: "七天之外", updatedAt: new Date(now - 8 * 86400000).toISOString(), running: false });
	const loaded = new Set<string>();
	const reads: string[] = [];
	const clients = new Set<ServerResponse>();
	let slow = false,
		completed = false;
	const json = (res: ServerResponse, value: unknown) => {
		res.writeHead(200, { "Content-Type": "application/json", Connection: "close" });
		res.end(JSON.stringify(value));
	};
	const detail = (id: string) => {
		const user = { role: "user", content: `问题 ${id}`, timestamp: now - 10000 };
		const message = {
			role: "assistant",
			timestamp: now - 9000,
			content:
				id === "s0"
					? [
							{ type: "thinking", thinking: "核对系统状态" },
							{
								type: "toolCall",
								id: "disk",
								name: "disk",
								arguments: { scope: "space" },
								execution: { startedAt: now },
							},
							{
								type: "toolCall",
								id: "ls",
								name: "ls",
								arguments: { path: "C:/" },
								execution: { startedAt: now },
							},
							{
								type: "toolCall",
								id: "plan",
								name: "command_plan",
								arguments: { title: "排查方案", level: "safe", items: [] },
								execution: { startedAt: now },
							},
						]
					: [{ type: "text", text: `正文 ${id}` }],
			...(id !== "s0" ? { stopReason: "stop" } : {}),
		};
		return {
			id,
			messages: [user, message],
			entries: [
				{ id: "u", message: user },
				{ id: "a", message },
			],
			running: id === "s0" && !completed,
			queue: { version: 0, items: [], paused: false },
			ui: [],
		};
	};
	const port = await freePort();
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions: rows, stage: "preload-test" }),
		async (_req, res, path) => {
			if (!path.startsWith("/api/")) return false;
			if (path.endsWith("/events")) {
				res.writeHead(200, { "Content-Type": "text/event-stream" });
				res.write(": connected\n\n");
				clients.add(res);
				res.on("close", () => clients.delete(res));
				return true;
			}
			const match = /^\/api\/sessions\/([^/]+)$/.exec(path);
			if (match) {
				const id = match[1];
				reads.push(id);
				await new Promise((r) => setTimeout(r, slow ? 1800 : 80));
				json(res, detail(id));
				loaded.add(id);
				return true;
			}
			if (path === "/api/sessions") json(res, { sessions: rows });
			else if (path === "/api/models") json(res, { models: [], enabled: null, selected: null, thinkingLevels: [] });
			else if (path === "/api/settings") json(res, { theme: "light", providers: [], models: [], scopedModels: [] });
			else if (path === "/api/account")
				json(res, {
					providerId: "cstoa",
					signedIn: false,
					requiresLogin: false,
					profile: { supported: true, studentId: null, name: null },
				});
			else if (path === "/api/auth") json(res, { providers: [] });
			else json(res, {});
			return true;
		},
	);
	await listenWebServer(server, port);
	let browser: BrowserProbe | undefined;
	try {
		browser = await BrowserProbe.launch(root);
		await browser.navigate(`http://127.0.0.1:${port}/`);
		for (let attempt = 0; attempt < 150 && loaded.size < 10; attempt++) await new Promise((r) => setTimeout(r, 30));
		assert.deepEqual(
			[...loaded].sort(),
			Array.from({ length: 10 }, (_, i) => `s${i}`).sort(),
			"自动预载严格限定十个近期会话",
		);
		await browser.until("!!document.querySelector('.workspace-content')", "预载后页面保持可用");
		slow = true;
		await browser.evaluate(
			"window.__cacheFrames=[];new MutationObserver(()=>{const id=location.pathname.split('/')[2];if(id)window.__cacheFrames.push({id,shown:document.querySelector('.chat-loaded')?.getAttribute('data-session-id'),loading:document.body.textContent.includes('正在读取会话')});}).observe(document.querySelector('.workspace-content'),{subtree:true,childList:true});",
		);
		for (const id of ["s1", "s2", "s1", "s0"]) {
			const elapsed = await browser.evaluate<number>(
				`(async()=>{const start=performance.now();document.querySelector('[data-session-id="${id}"].sidebar-session').click();for(let i=0;i<20;i++){await new Promise(r=>requestAnimationFrame(r));if(document.querySelector('.chat-loaded')?.dataset.sessionId==='${id}')return performance.now()-start;}return 9999;})()`,
			);
			assert.ok(elapsed < 600, `${id} 切换 ${elapsed}ms，不能等待1800ms的刷新`);
		}
		assert.equal(
			await browser.evaluate("window.__cacheFrames.some(f=>f.loading || (f.shown && f.id!==f.shown))"),
			false,
			"切换无读取文字或旧正文",
		);
		await browser.until(
			"document.querySelectorAll('.running-light[data-running]').length>=3 && !!document.querySelector('.running-corner[data-running]')",
			"运行中扫光",
		);
		await browser.evaluate(
			"Promise.all(document.getAnimations().filter(a=>a.effect?.getTiming().iterations!==Infinity).map(a=>a.finished.catch(()=>{})))",
		);
		const motion = await browser.evaluate<{
			text: string;
			blend: string;
			corner: string;
			width: number;
			card: number;
		}>(
			"(()=>{const t=document.querySelector('.running-light[data-running]'),c=document.querySelector('.running-corner[data-running]');return {text:getComputedStyle(t,'::after').animationName,blend:getComputedStyle(t,'::after').mixBlendMode,corner:getComputedStyle(c,'::after').animationName,width:c.getBoundingClientRect().width,card:c.parentElement.getBoundingClientRect().width};})()",
		);
		assert.equal(motion.text, "running-light-sweep");
		assert.equal(motion.blend, "soft-light");
		assert.equal(motion.corner, "running-light-sweep");
		assert.ok(motion.width < motion.card / 2);
		await browser.screenshot(join(root, "running-light.png"));
		await browser.evaluate("document.documentElement.dataset.theme='dark'");
		await browser.screenshot(join(root, "running-dark.png"));
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 390,
			height: 900,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await browser.evaluate("document.querySelector('[aria-label=\"收起侧栏\"]')?.click()");
		await browser.evaluate(
			"Promise.all(document.getAnimations().filter(a=>a.effect?.getTiming().iterations!==Infinity).map(a=>a.finished.catch(()=>{})))",
		);
		await browser.screenshot(join(root, "running-mobile.png"));
		await browser.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "reduce" }],
		});
		assert.equal(
			await browser.evaluate("getComputedStyle(document.querySelector('.running-light'),'::after').display"),
			"none",
		);
		assert.equal(
			await browser.evaluate("getComputedStyle(document.querySelector('.running-corner'),'::after').display"),
			"none",
		);
		completed = true;
		for (const res of clients) res.write('event: session\ndata: {"type":"agent_end"}\n\n');
		await browser.until(
			"!document.querySelector('.running-light[data-running], .running-corner[data-running]')",
			"完成后停止扫光",
		);
		await writeFile(join(root, "evidence.json"), JSON.stringify({ loaded: [...loaded], reads, motion }, null, 2));
		console.log("PRELOAD_EVIDENCE", root);
	} finally {
		await browser?.close();
		for (const res of clients) res.end();
		server.closeAllConnections();
		await new Promise<void>((r) => server.close(() => r()));
	}
});
