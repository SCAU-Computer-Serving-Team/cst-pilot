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
	const assets: string[] = [];
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
			if (path.startsWith("/assets/") && /\.(js|css)$/.test(path)) assets.push(path);
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
		await browser.until(
			`(() => {
				const links = [...document.querySelectorAll('link[rel="modulepreload"][href*="/chat-"]')];
				const loaded = new Set(performance.getEntriesByType('resource').map(e => e.name));
				return links.length > 0 && links.every(link => loaded.has(link.href));
			})()`,
			"点击前路由入口及依赖已加载",
		);
		const assetsBeforeNavigation = assets.length;
		slow = true;
		await browser.evaluate(`(() => {
			window.__sessionTransitionCount = 0;
			const original = document.startViewTransition.bind(document);
			document.startViewTransition = (...args) => {
				window.__sessionTransitionCount++;
				return original(...args);
			};
		})()`);
		await browser.evaluate(
			"window.__cacheFrames=[];new MutationObserver(()=>{const id=location.pathname.split('/')[2];if(id)window.__cacheFrames.push({id,shown:document.querySelector('.chat-loaded')?.getAttribute('data-session-id'),loading:document.body.textContent.includes('正在读取会话')});}).observe(document.querySelector('.workspace-content'),{subtree:true,childList:true});",
		);
		const switchTimings: { id: string; elapsed: number }[] = [];
		for (const id of ["s1", "s2", "s1", "s0"]) {
			const elapsed = await browser.evaluate<number>(
				`(async()=>{const start=performance.now();document.querySelector('[data-session-id="${id}"].sidebar-session').click();for(let i=0;i<20;i++){await new Promise(r=>requestAnimationFrame(r));if(document.querySelector('.chat-loaded')?.dataset.sessionId==='${id}')return performance.now()-start;}return 9999;})()`,
			);
			switchTimings.push({ id, elapsed });
			assert.ok(elapsed < 600, `${id} 切换 ${elapsed}ms，不能等待1800ms的刷新`);
		}
		assert.deepEqual(assets.slice(assetsBeforeNavigation), [], "首次及重复切换均无需请求额外 JS 或 CSS");
		assert.equal(await browser.evaluate("window.__sessionTransitionCount"), 0, "侧栏切换会话不等待页面快照");
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
		assert.ok(
			((await browser.evaluate("document.querySelectorAll('.running-glyph').length")) as number) > 9,
			"状态按完整字素分别渲染",
		);
		const sampleGlyphs = () =>
			browser!.evaluate<{ strengths: number[]; brightness: number[] }>(
				`(()=>{const line=[...document.querySelectorAll('.running-light[data-running]')].find(e=>e.querySelectorAll('.running-glyph').length>=9);window.__glyphLine=line;const glyphs=[...line.querySelectorAll('.running-glyph')],center=4;for(const glyph of glyphs)for(const a of glyph.getAnimations()){a.pause();a.currentTime=a.effect.getTiming().duration*(center+3)/(glyphs.length+5);}const canvas=document.createElement('canvas'),ctx=canvas.getContext('2d');return {strengths:glyphs.map(e=>parseFloat(getComputedStyle(e).getPropertyValue('--running-glyph-strength'))),brightness:glyphs.map(e=>{ctx.fillStyle=getComputedStyle(e).color;ctx.fillRect(0,0,1,1);return [...ctx.getImageData(0,0,1,1).data].slice(0,3).reduce((a,b)=>a+b,0);})};})()`,
			);
		const lightGlyphs = await sampleGlyphs();
		assert.deepEqual(
			lightGlyphs.strengths.map((v) => Math.round(v * 100)),
			[0, 0, 8, 24, 48, 24, 8, ...Array(lightGlyphs.strengths.length - 7).fill(0)],
		);
		assert.ok(
			lightGlyphs.brightness[4] > lightGlyphs.brightness[3] &&
				lightGlyphs.brightness[3] > lightGlyphs.brightness[2] &&
				lightGlyphs.brightness[2] > lightGlyphs.brightness[1],
			"浅色中心字最亮，旁边两档递减",
		);
		const motion = await browser.evaluate<{ corner: string; mask: string; width: number; card: number }>(
			"(()=>{const c=document.querySelector('.running-corner[data-running]');return {corner:getComputedStyle(c,'::after').animationName,mask:getComputedStyle(c).maskImage,width:c.getBoundingClientRect().width,card:c.parentElement.getBoundingClientRect().width};})()",
		);
		assert.equal(motion.corner, "running-light-sweep");
		assert.ok(Math.abs(motion.width - motion.card) < 3, "柔边三角形按卡片整体比例计算");
		assert.ok(!motion.mask.includes("radial-gradient"), "角落光影使用斜向渐淡，消除椭圆边缘");
		const mask = await browser.evaluate<{ area: number; near: number; middle: number; far: number }>(
			`(async()=>{const c=document.querySelector('.running-corner'),url=getComputedStyle(c).maskImage.slice(4,-1).replace(/^"|"$/g,''),img=new Image();img.src=url;await img.decode();const canvas=document.createElement('canvas');canvas.width=canvas.height=200;const ctx=canvas.getContext('2d');ctx.drawImage(img,0,0,200,200);const data=ctx.getImageData(0,0,200,200).data;let covered=0;for(let i=3;i<data.length;i+=4)if(data[i]>1)covered++;const alpha=(x,y)=>data[(y*200+x)*4+3]/255;return {area:covered/40000,near:alpha(198,198),middle:alpha(160,160),far:alpha(80,80)};})()`,
		);
		assert.ok(mask.area > 0.32 && mask.area < 0.45, `柔化三角形覆盖比例 ${mask.area}`);
		assert.ok(mask.near > mask.middle && mask.middle > mask.far && mask.far === 0, "距离右下角越远越淡");
		await browser.evaluate(
			"for(const a of document.querySelector('.running-corner').getAnimations({subtree:true})){a.pause();a.currentTime=Number(a.effect.getTiming().duration)*2/3;}",
		);
		await browser.screenshot(join(root, "running-light.png"));
		await browser.evaluate("document.documentElement.dataset.theme='dark'");
		const darkGlyphs = await sampleGlyphs();
		assert.ok(
			darkGlyphs.brightness[4] < darkGlyphs.brightness[3] &&
				darkGlyphs.brightness[3] < darkGlyphs.brightness[2] &&
				darkGlyphs.brightness[2] < darkGlyphs.brightness[1],
			"深色中心字最暗，旁边两档递减",
		);
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
		await browser.until(
			"document.querySelectorAll('.running-glyph').length && [...document.querySelectorAll('.running-glyph')].every(e=>e.getAnimations().length===0 && parseFloat(getComputedStyle(e).getPropertyValue('--running-glyph-strength'))===0)",
			"减少动态时取消逐字光照并恢复原色",
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
		await writeFile(
			join(root, "evidence.json"),
			JSON.stringify({ loaded: [...loaded], reads, assets, switchTimings, motion }, null, 2),
		);
		console.log("PRELOAD_EVIDENCE", root);
	} finally {
		await browser?.close();
		for (const res of clients) res.end();
		server.closeAllConnections();
		await new Promise<void>((r) => server.close(() => r()));
	}
});
