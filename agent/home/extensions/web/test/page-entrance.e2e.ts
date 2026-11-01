import assert from "node:assert/strict";
import { mkdtemp, writeFile } from "node:fs/promises";
import type { ServerResponse } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

type Entrance = { name: string; target: string };
type SidebarFrame = { time: number; rows: number; opacity: number; visibility: string; allTop: number };
type SelectionFrame = {
	time: number;
	error: number;
	sidebarOpacity: number;
	sidebarLeft: number;
	rowBackground: string;
};

test("刷新重播入场，保留地址与侧栏偏好，站内切页不补播，减少动态直接呈现", { timeout: 60_000 }, async () => {
	const root = await mkdtemp(join(await testRoot(), "page-entrance-"));
	const clients = new Set<ServerResponse>();
	const rows = Array.from({ length: 20 }, (_, index) => ({
		id: `s${index}`,
		title: "你好",
		updatedAt: new Date(Date.now() - index * 1000).toISOString(),
		running: false,
	}));
	let theme = "light";
	let stateDelay = 0;
	let emptyList = false;
	let failState = false;
	const port = await freePort();
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		async () => {
			if (stateDelay) await new Promise((resolve) => setTimeout(resolve, stateDelay));
			if (failState) throw new Error("测试列表读取失败");
			return { sessions: emptyList ? [] : rows, stage: "entrance-test" };
		},
		async (_req, res, path) => {
			if (!path.startsWith("/api/")) return false;
			if (path.endsWith("/events")) {
				res.writeHead(200, { "Content-Type": "text/event-stream" });
				res.write(": connected\n\n");
				clients.add(res);
				res.on("close", () => clients.delete(res));
				return true;
			}
			let data: unknown = {};
			if (path === "/api/sessions") data = { sessions: emptyList ? [] : rows };
			else if (/^\/api\/sessions\/s\d+$/.test(path))
				data = {
					id: path.split("/").at(-1),
					messages: [],
					entries: [],
					running: false,
					queue: { version: 0, items: [], paused: false },
					ui: [],
				};
			else if (path === "/api/models") data = { models: [], enabled: null, selected: null, thinkingLevels: [] };
			else if (path === "/api/settings") data = { theme, providers: [], models: [], scopedModels: [] };
			else if (path === "/api/account") data = { signedIn: false, requiresLogin: false };
			else if (path === "/api/auth") data = { providers: [] };
			res.writeHead(200, { "Content-Type": "application/json", Connection: "close" });
			res.end(JSON.stringify(data));
			return true;
		},
	);
	await listenWebServer(server, port);
	let browser: BrowserProbe | undefined;
	try {
		browser = await BrowserProbe.launch(root);
		const probe = browser;
		const origin = `http://127.0.0.1:${port}`;
		await probe.call("Page.addScriptToEvaluateOnNewDocument", {
			source: `window.__entranceDocument=performance.timeOrigin; window.__entrances=[]; window.__selectionFrames=[]; window.__sidebarFrames=[];
				const recordSelection = () => {
					const sidebar=document.querySelector('.sidebar');
					const top=sidebar?.querySelector('.sidebar-top');
					if(top) window.__sidebarFrames.push({time:performance.now(),rows:top.querySelectorAll('.sidebar-session').length,
						opacity:Number(getComputedStyle(top).opacity),visibility:getComputedStyle(top).visibility,
						allTop:top.querySelector('.sidebar-all')?.getBoundingClientRect().top});
					const row=sidebar?.querySelector('.sidebar-session.selected');
					const pill=sidebar?.querySelector('.sidebar-selection');
					if (row && pill && getComputedStyle(pill).visibility==='visible') {
						const target=row.getBoundingClientRect(), selection=pill.getBoundingClientRect();
						window.__selectionFrames.push({time:performance.now(),error:Math.abs(target.top-selection.top),
							sidebarOpacity:Number(getComputedStyle(sidebar).opacity),
							sidebarLeft:sidebar.getBoundingClientRect().left,
							rowBackground:getComputedStyle(row).backgroundColor});
					}
					if(performance.now()<5000) requestAnimationFrame(recordSelection);
				};
				requestAnimationFrame(recordSelection);
				document.addEventListener('animationstart', event => {
					if (event.target instanceof Element && event.target.closest('.app-layout'))
						window.__entrances.push({name:event.animationName,target:event.target.getAttribute('class') ?? event.target.tagName});
				}, true);`,
		});
		const evidence: Record<string, Entrance[]> = {};
		async function reload() {
			const before = await probe.evaluate<number>("window.__entranceDocument");
			await probe.call("Page.reload", { ignoreCache: true });
			await probe.until(
				`window.__entranceDocument !== ${before} && document.readyState === 'complete' && !!document.querySelector('.app-layout')`,
				"刷新后的新文档就绪",
			);
		}
		async function homeEntrance(label: string, sidebar = true) {
			await probe.until(
				"window.__entrances.some(e=>e.name==='startup-bottom' && e.target.includes('composer-shell--home'))",
				`${label} 输入框入场`,
			);
			const entries = await probe.evaluate<Entrance[]>("window.__entrances");
			for (const target of ["brand-arcs", "home-greeting", "composer-shell--home", "home-footnote"]) {
				const selector = target === "home-greeting" ? ".home-greeting h1" : `.${target}`;
				assert.match(
					await probe.evaluate<string>(`getComputedStyle(document.querySelector('${selector}')).animationName`),
					/^startup-(top|bottom)$/,
					`${label} ${target} 使用分层入场`,
				);
			}
			assert.equal(
				entries.some((entry) => entry.name === "startup-left"),
				sidebar,
				`${label} 侧栏入场`,
			);
			assert.equal(
				await probe.evaluate("getComputedStyle(document.querySelector('.home-backdrop')).animationName"),
				"none",
				"背景不参与页面位移",
			);
			evidence[label] = entries;
		}
		async function settled() {
			await probe.until("!document.querySelector('[data-home-startup], [data-startup-rows]')", "入场结束");
		}
		await probe.navigate(`${origin}/?entrance=1#keep-anchor`);
		await homeEntrance("首次打开");
		assert.equal(await probe.evaluate("location.search+location.hash"), "?entrance=1#keep-anchor");
		await settled();
		await reload();
		await homeEntrance("刷新首页");
		await probe.evaluate(`for(const animation of document.getAnimations()) {
			if (!animation.animationName?.startsWith('startup-')) continue;
			animation.pause();
			const timing=animation.effect.getTiming();
			animation.currentTime=timing.delay+Number(timing.duration)*.2;
		}`);
		await probe.screenshot(join(root, "home-refresh-entrance.png"));
		await probe.evaluate("for(const a of document.getAnimations()) if(a.playState==='paused') a.play()");
		await settled();
		await reload();
		await probe.until("!!document.querySelector('.sidebar-session')", "首屏会话行就绪");
		await probe.evaluate("document.querySelector('.sidebar-session').click()");
		await probe.until("location.pathname==='/s/s0' && !!document.querySelector('.chat-loaded')", "入场期间切入会话");
		const beforeReturn = await probe.evaluate<number>("window.__entrances.length");
		await probe.evaluate("document.querySelector('.product-name').click()");
		await probe.until(
			"location.pathname==='/' && !!document.querySelector('.home-main') && !document.querySelector('[data-home-transition]')",
			"站内返回首页",
		);
		assert.equal(await probe.evaluate("!!document.querySelector('[data-home-startup], [data-startup]')"), false);
		assert.deepEqual(
			await probe.evaluate(`window.__entrances.slice(${beforeReturn}).filter(e=>e.name.startsWith('startup-'))`),
			[],
			"站内导航沿用页面转场，不重播整页入场",
		);
		theme = "dark";
		stateDelay = 1600;
		await probe.call("Network.setCookie", { name: "cst-theme", value: "dark", url: origin });
		await probe.navigate(`${origin}/s/s2`);
		await probe.until("window.__entrances.some(e=>e.name==='startup-left')", "直接打开会话时侧栏入场");
		await reload();
		await probe.until("window.__entrances.some(e=>e.name==='startup-left')", "刷新会话时侧栏入场");
		await probe.until("!!document.querySelector('.chat-composer-area')", "聊天输入区就绪");
		await probe.until("document.querySelectorAll('.sidebar-session').length===20", "慢速列表完整返回");
		await settled();
		const sidebarFrames = await probe.evaluate<SidebarFrame[]>("window.__sidebarFrames");
		await writeFile(join(root, "sidebar-first-frames.json"), JSON.stringify(sidebarFrames, null, 2));
		assert.ok(
			sidebarFrames.some((frame) => frame.rows === 0),
			"覆盖列表返回前的首屏阶段",
		);
		assert.ok(
			sidebarFrames.every((frame) => frame.rows === 20 || frame.opacity === 0 || frame.visibility !== "visible"),
			"首帧起不显示空的最近列表，避免查看全部先出现再跳位",
		);
		const visiblePositions = sidebarFrames
			.filter((frame) => frame.opacity > 0 && frame.visibility === "visible")
			.map((frame) => frame.allTop);
		assert.ok(visiblePositions.length > 10, "采集列表可见后的连续帧");
		assert.ok(
			Math.max(...visiblePositions) - Math.min(...visiblePositions) < 1,
			"查看全部首次出现后不再因补齐列表发生纵向跳动",
		);
		evidence.chatRefresh = await probe.evaluate<Entrance[]>("window.__entrances");
		const frames = await probe.evaluate<SelectionFrame[]>("window.__selectionFrames");
		await writeFile(join(root, "selection-frames.json"), JSON.stringify(frames, null, 2));
		await probe.screenshot(join(root, "chat-refresh-selection.png"));
		assert.ok(frames.length > 10, "采集刷新入场各阶段的选区位置");
		assert.ok(
			frames.every((frame) => frame.error < 1),
			`选中背景始终贴合会话行，最大偏差 ${Math.max(...frames.map((frame) => frame.error))}px`,
		);
		assert.ok(
			frames.every((frame) => frame.sidebarOpacity === 1 && Math.abs(frame.sidebarLeft) < 1),
			"侧栏底色保持不透明和静止，入场不闪白",
		);
		assert.ok(
			frames.every((frame) => frame.rowBackground === "rgba(0, 0, 0, 0)"),
			"选中会话不叠加第二层背景",
		);
		stateDelay = 0;
		await probe.evaluate("localStorage.setItem('cst-ui-sidebar','1')");
		await probe.call("Emulation.setDeviceMetricsOverride", {
			width: 390,
			height: 844,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await probe.navigate(`${origin}/`);
		await homeEntrance("窄屏收起侧栏", false);
		assert.equal(await probe.evaluate("document.querySelector('.sidebar').inert"), true);
		await settled();
		await probe.screenshot(join(root, "home-refresh-mobile.png"));
		await probe.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "reduce" }],
		});
		await reload();
		assert.equal(
			await probe.evaluate(
				"[...document.querySelectorAll('.brand-arcs,.home-greeting h1,.composer-shell--home,.home-footnote,.sidebar')].every(e=>getComputedStyle(e).animationName==='none')",
			),
			true,
			"减少动态直接呈现",
		);
		assert.equal(
			await probe.evaluate("getComputedStyle(document.querySelector('.composer-shell--home')).opacity"),
			"1",
		);
		assert.equal(await probe.evaluate("document.documentElement.scrollWidth<=innerWidth"), true);
		assert.equal(await probe.evaluate("localStorage.getItem('cst-ui-sidebar')"), "1");
		emptyList = true;
		stateDelay = 250;
		await probe.evaluate("localStorage.setItem('cst-ui-sidebar','0')");
		await reload();
		await probe.until(
			"!!document.querySelector('.sidebar-empty') && !document.querySelector('[data-startup-pending]')",
			"真实空列表正常呈现",
		);
		assert.equal(await probe.evaluate("document.querySelector('.sidebar').inert"), false);
		emptyList = false;
		failState = true;
		await reload();
		await probe.until(
			"!!document.querySelector('.sidebar-error') && !document.querySelector('[data-startup-pending]')",
			"读取失败不持续隐藏侧栏",
		);
		assert.equal(await probe.evaluate("document.querySelector('.sidebar').inert"), false);
		failState = false;
		await reload();
		await probe.until("document.querySelectorAll('.sidebar-session').length===20", "失败后刷新恢复完整列表");
		await writeFile(join(root, "evidence.json"), JSON.stringify(evidence, null, 2));
		console.log("PAGE_ENTRANCE_EVIDENCE", root);
	} finally {
		await browser?.close();
		for (const client of clients) client.end();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	}
});
