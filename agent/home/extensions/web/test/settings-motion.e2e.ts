import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

test("设置控件、紧凑协议菜单及外部入场动效", { timeout: 60_000 }, async () => {
	const root = join(await testRoot(), "settings-motion");
	await mkdir(root, { recursive: true });
	const home = await mkdtemp(join(root, "home-"));
	await writeFile(join(home, "settings.json"), JSON.stringify({ theme: "light" }));
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
		() => ({ sessions: [], stage: "motion-test" }),
		api,
	);
	let browser: BrowserProbe | undefined;
	try {
		await listenWebServer(server, port);
		browser = await BrowserProbe.launch(root);
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 1440,
			height: 1100,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await browser.navigate(`${origin}/settings/custom`);
		await browser.click(".settings-custom-select .select__trigger");
		await browser.until(
			"!!document.querySelector('.settings-protocol-popover') && !document.querySelector('.settings-protocol-popover[data-entering]')",
			"协议菜单展开完毕",
		);
		const menu = await browser.evaluate<{
			width: number;
			height: number;
			rows: number[];
			top: number;
			bottom: number;
		}>(
			"(()=>{const n=document.querySelector('.settings-protocol-popover'),r=n.getBoundingClientRect(),items=[...n.querySelectorAll('[role=option]')].map(i=>i.getBoundingClientRect());return {width:r.width,height:r.height,rows:items.map(i=>i.height),top:items[0].top-r.top,bottom:r.bottom-items.at(-1).bottom}})()",
		);
		assert.ok(menu.width <= 360, `协议菜单限制宽度，避免继承触发框宽度：${JSON.stringify(menu)}`);
		assert.deepEqual(menu.rows, [32, 32, 32, 32]);
		assert.ok(
			menu.top <= 9 && menu.bottom <= 9 && Math.abs(menu.top - menu.bottom) < 1,
			"菜单上下留白相等，单层内边距",
		);
		await browser.call("Input.dispatchKeyEvent", { type: "keyDown", key: "Escape", code: "Escape" });
		assert.equal(
			await browser.evaluate(
				"[...document.querySelectorAll('.settings-custom-field > input, .settings-custom-key')].every(n=>Math.round(n.getBoundingClientRect().height)===42)&&getComputedStyle(document.querySelector('.settings-custom-key input')).fontSize==='16px'",
			),
			true,
			"输入框 42px / 16px",
		);
		assert.equal(
			await browser.evaluate(
				"Math.round(document.querySelector('.settings-custom-select .select__trigger').getBoundingClientRect().height)",
			),
			32,
			"协议选择器保持 32px",
		);
		assert.equal(
			await browser.evaluate(
				"getComputedStyle(document.querySelector('.settings-custom-field input'),'::placeholder').color===getComputedStyle(document.querySelector('.settings-custom-field small')).color",
			),
			true,
			"占位文字沿用完整不透明度的说明色",
		);
		assert.equal(
			await browser.evaluate(
				"document.querySelectorAll('.settings-custom-capabilities > .settings-capability-row').length",
			),
			2,
			"能力项为两条文字与独立按钮行",
		);
		await browser.call("Page.addScriptToEvaluateOnNewDocument", {
			source:
				"window.__motion=[];document.addEventListener('animationstart',e=>window.__motion.push({name:e.animationName,target:e.target.className}));",
		});
		await browser.navigate(`${origin}/`);
		await browser.evaluate(
			`(() => {const start=document.startViewTransition.bind(document);document.startViewTransition=(...args)=>{const t=start(...args);t.ready.then(()=>{window.__entrySnapshots=document.getAnimations().map(a=>a.effect?.pseudoElement)});return t}})()`,
		);
		await browser.click(".sidebar-account-trigger");
		await browser.click('.sidebar-account-item[href="/settings"]');
		await browser.until(
			"location.pathname==='/settings'&&window.__entrySnapshots?.includes('::view-transition-new(settings-surface)')",
			"外部进入设置使用页面快照",
		);
		await browser.until("!document.getAnimations().some(a=>a.playState==='running')", "设置入场结束");
		await browser.evaluate("window.__motion=[]");
		await browser.click('a[href="/settings/general"]');
		await browser.until(
			"location.pathname==='/settings/general'&&document.getAnimations().some(a=>a.effect?.pseudoElement==='::view-transition-new(settings-page)')",
			"内部切页快照",
		);
		assert.equal(
			await browser.evaluate("document.querySelector('.app-layout').hasAttribute('data-route-entry')"),
			false,
			"内部快照切页不叠加局部入场",
		);
		await browser.navigate(`${origin}/settings/add`);
		await browser.until(
			"!!document.querySelector('.settings-service-card .settings-models-toggle')",
			"可展开的服务列表",
		);
		const resize = await browser.evaluate<{ start: number; middle: number; end: number; duration: number }>(
			"(async()=>{const n=document.querySelector('.settings-service-card'),start=n.getBoundingClientRect().height,original=Element.prototype.animate;let captured;Element.prototype.animate=function(frames,timing){const a=original.call(this,frames,timing);if(this===n){captured=a;a.pause();a.currentTime=125}return a};try{n.querySelector('.settings-models-toggle').click();for(let i=0;i<20&&!captured;i++)await new Promise(r=>setTimeout(r,10));const duration=captured?.effect.getTiming().duration,middle=n.getBoundingClientRect().height;captured?.finish();await captured?.finished;return {start,middle,end:n.getBoundingClientRect().height,duration}}finally{Element.prototype.animate=original}})()",
		);
		assert.equal(resize.duration, 250, "列表高度使用 250ms 过渡");
		assert.ok(resize.middle > resize.start && resize.middle < resize.end, JSON.stringify(resize));
		await browser.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "reduce" }],
		});
		await browser.navigate(`${origin}/settings/custom`);
		assert.equal(
			await browser.evaluate(
				"[...document.querySelectorAll('.settings-page-heading,.settings-section')].every(n=>getComputedStyle(n).animationName==='none'&&getComputedStyle(n).opacity==='1')",
			),
			true,
			"减少动态保持内容可见",
		);
		await browser.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "no-preference" }],
		});
		await browser.call("Page.addScriptToEvaluateOnNewDocument", { source: "document.startViewTransition=undefined" });
		await browser.navigate(`${origin}/settings/general`);
		await browser.until("window.__motion.some(e=>e.name==='route-part-in')", "不支持快照时仍有局部入场");
	} finally {
		await browser?.close();
		await api.close();
		await pool.close();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		await rm(home, { recursive: true, force: true });
	}
});
