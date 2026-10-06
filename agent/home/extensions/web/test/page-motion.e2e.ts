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

type Snapshot = {
	opacity: number;
	x: number;
	y: number;
	mask: string;
	erasure: string;
	blur: number;
	group: string;
	duration: number;
	delay: number;
};
type Frame = Record<string, Snapshot>;

/** 在真实过渡的 ready 回调中同帧采样，截图耗时不参与采样时间。 */
async function transition(browser: BrowserProbe, selector: string, direction: string, root: string) {
	if (await browser.evaluate("document.querySelector('.sidebar')?.inert")) {
		await browser.click('[aria-label="展开侧栏"]');
		await browser.evaluate("Promise.all(document.querySelector('.sidebar').getAnimations().map(a=>a.finished))");
	}
	await browser.evaluate(`window.__motion=null;window.__pauseNext=true;window.__captureMotion=(t)=>{
		const animations=document.getAnimations().filter(a=>a.effect?.pseudoElement&&a.playState!=='finished');
		function frame(time){
			animations.forEach(a=>a.currentTime=time);
			const result={};for(const name of ['home-background','home-curtain-left','home-curtain-right','home-mark','home-greeting','home-composer','home-footer','workspace-composer']){
				const side=name.startsWith('home-curtain-')?'new':name==='workspace-composer'?${JSON.stringify(direction === "to-home" ? "old" : "new")}:${JSON.stringify(direction === "to-home" ? "new" : "old")};
				const s=getComputedStyle(document.documentElement,'::view-transition-'+side+'('+name+')');
				const g=getComputedStyle(document.documentElement,'::view-transition-group('+name+')');
				const timing=animations.find(a=>a.effect.pseudoElement==='::view-transition-'+side+'('+name+')')?.effect.getTiming();
				result[name]={opacity:parseFloat(s.opacity),x:s.transform==='none'?0:new DOMMatrixReadOnly(s.transform).m41,mask:s.maskImage,erasure:s.getPropertyValue('--curtain-erasure'),y:s.transform==='none'?0:new DOMMatrixReadOnly(s.transform).m42,blur:s.filter.startsWith('blur(')?parseFloat(s.filter.slice(5)):0,group:g.transform,duration:Number(timing?.duration),delay:timing?.delay??0};
			}return result;
		}
		const curtains=[...document.querySelectorAll('.home-exit-curtain')].map(e=>{const c=e.querySelector('canvas'),r=e.getBoundingClientRect();return {left:r.left,top:r.top,width:r.width,height:r.height,background:getComputedStyle(e).backgroundImage,bitmapWidth:c.width,alpha:c.getContext('2d').getImageData(Math.floor(c.width/2),Math.floor(c.height/2),1,1).data[3]};});
		const start=frame(0),middle=frame(80),late=frame(400),end=frame(600);
		window.__motion={transition:t,animations,frames:{start,middle,late,end},curtains};
	};`);
	await browser.click(selector);
	await browser.until("!!window.__motion", `${direction} 产生原生过渡`);
	const frames = await browser.evaluate<{ start: Frame; middle: Frame; late: Frame; end: Frame }>(
		"window.__motion.frames",
	);
	if (direction === "to-workspace") {
		const curtains =
			await browser.evaluate<
				{
					left: number;
					top: number;
					width: number;
					height: number;
					background: string;
					bitmapWidth: number;
					alpha: number;
				}[]
			>("window.__motion.curtains");
		assert.equal(curtains.length, 2);
		assert.ok(
			Math.abs(curtains[0].left + curtains[0].width - 32 - (curtains[1].left + 32)) < 1,
			"两片以主区中线切分",
		);
		for (const slice of curtains) {
			assert.ok(slice.width > 0 && slice.height > 0);
			assert.ok(slice.background.includes("gradient"), "没有WebGL首帧时使用同一CSS背景");
			if (slice.bitmapWidth > 1) assert.equal(slice.alpha, 255, "WebGL帧实际复制到背景片");
		}
		await writeFile(join(root, `${direction}-geometry.json`), JSON.stringify(curtains, null, 2));
	}
	await browser.evaluate("window.__motion.transition.finished");
	await browser.until("!document.querySelector('[data-home-transition]')", `${direction} 路由过渡清理`);
	await browser.evaluate(
		"Promise.all([...document.querySelectorAll('.sidebar,.home-center,.home-footnote,.home-backdrop,.sidebar-restore')].flatMap(e=>e.getAnimations()).filter(a=>a.playState==='running').map(a=>a.finished))",
	);
	await browser.screenshot(join(root, `${direction}-end.png`));
	return frames;
}

test("双向页面动效：聊天框各自在原位下方入退场，页脚与背景衔接", { timeout: 60_000 }, async () => {
	const root = join(await testRoot(), "page-motion");
	await mkdir(root, { recursive: true });
	const home = await mkdtemp(join(root, "home-"));
	await writeFile(
		join(home, "settings.json"),
		JSON.stringify({ theme: "light", defaultProvider: "probe", defaultModel: "one", enabledModels: ["probe/one"] }),
	);
	await writeFile(
		join(home, "models.json"),
		JSON.stringify({
			providers: {
				probe: {
					baseUrl: "http://127.0.0.1:1/v1",
					api: "openai-completions",
					apiKey: "test-only",
					models: [
						{
							id: "one",
							name: "动效验收模型",
							reasoning: false,
							input: ["text"],
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
		const response = await fetch(`http://127.0.0.1:${port}/api/sessions`, {
			method: "POST",
			headers: {
				"Content-Type": "application/json",
				"X-CST-Web-Request": "1",
				"Idempotency-Key": "motion-session",
				Origin: `http://127.0.0.1:${port}`,
			},
			body: "{}",
		});
		assert.equal(response.status, 201);
		const { id } = await response.json();
		browser = await BrowserProbe.launch(root);
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 1600,
			height: 1000,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await browser.navigate(`http://127.0.0.1:${port}/s/${id}`);
		await browser.until(
			"!!document.querySelector('.sidebar-new') && !!document.querySelector('.composer-input')",
			"工作台加载",
		);
		await browser.evaluate(
			`(()=>{const native=document.startViewTransition.bind(document);document.startViewTransition=(...args)=>{const t=native(...args);window.__latestMotionTransition=t;if(window.__pauseNext){window.__pauseNext=false;t.ready.then(()=>window.__captureMotion(t));}return t;};})()`,
		);
		const intoHome = await transition(browser, ".sidebar-new", "to-home", root);
		for (const name of ["home-background", "home-mark", "home-greeting", "home-composer", "home-footer"]) {
			assert.equal(intoHome.start[name].duration, 500, `返回主页 ${name} 使用 500ms 入场`);
		}
		assert.equal(intoHome.start["home-composer"].delay, 40);
		assert.equal(intoHome.start["home-footer"].delay, 80);
		assert.ok(
			intoHome.start["home-composer"].y > 0 && intoHome.start["home-composer"].opacity < 0.01,
			"首页聊天框从终点下方淡入",
		);
		assert.ok(
			intoHome.middle["home-composer"].y > 0 &&
				intoHome.middle["home-composer"].y < intoHome.start["home-composer"].y,
		);
		assert.ok(
			intoHome.middle["workspace-composer"].y > 0 && intoHome.middle["workspace-composer"].opacity < 1,
			"工作台聊天框向下淡出",
		);
		assert.equal(
			intoHome.start["workspace-composer"].group,
			intoHome.middle["workspace-composer"].group,
			"工作台聊天框快照不飞向中心",
		);
		assert.ok(intoHome.start["home-footer"].y > 0 && intoHome.start["home-footer"].opacity < 0.01, "页脚从下方缓入");
		assert.ok(
			intoHome.start["home-composer"].blur > 0 && intoHome.end["home-composer"].blur === 0,
			JSON.stringify(intoHome),
		);
		assert.equal(intoHome.end["home-composer"].opacity, 1);
		assert.equal(intoHome.end["home-composer"].y, 0);
		const intoWorkspace = await transition(browser, `.sidebar-session[href="/s/${id}"]`, "to-workspace", root);
		for (const name of ["home-curtain-left", "home-curtain-right"]) {
			assert.equal(intoWorkspace.start[name].duration, 500, "背景两片使用500ms退场");
			assert.ok(intoWorkspace.middle[name].mask.includes("linear-gradient"), "中缝采用柔化透明遮罩");
			assert.notEqual(intoWorkspace.middle[name].erasure, intoWorkspace.start[name].erasure, "内側遮罩先推进透明度");
			assert.equal(intoWorkspace.middle[name].opacity, 1, "外侧早期保持不透明");
			assert.ok(intoWorkspace.late[name].opacity < 1, "接近末尾时外侧才淡出");
			assert.equal(intoWorkspace.middle[name].y, 0, "背景不再上下移动");
			assert.equal(intoWorkspace.end[name].opacity, 0);
		}
		assert.ok(
			intoWorkspace.middle["home-curtain-left"].x < 0 && intoWorkspace.middle["home-curtain-right"].x > 0,
			"左右向外滑出",
		);
		assert.equal(intoWorkspace.middle["home-mark"].y, 0, "LOGO原位淡出");
		assert.equal(intoWorkspace.middle["home-greeting"].y, 0, "大字原位淡出");
		assert.equal(intoWorkspace.start["workspace-composer"].duration, 400, "进入工作区输入区仍为 400ms");
		assert.ok(
			intoWorkspace.middle["home-mark"].opacity < 1 && intoWorkspace.middle["home-greeting"].opacity < 1,
			"LOGO与大字淡出",
		);
		assert.ok(intoWorkspace.middle["home-composer"].y > 0 && intoWorkspace.middle["home-composer"].opacity < 1);
		assert.ok(
			intoWorkspace.start["workspace-composer"].y > 0 && intoWorkspace.start["workspace-composer"].opacity < 0.01,
			"工作台聊天框从下方淡入",
		);
		assert.equal(intoWorkspace.end["workspace-composer"].y, 0);
		assert.equal(intoWorkspace.end["workspace-composer"].opacity, 1);
		await browser.click(".sidebar-new");
		await browser.until(
			"location.pathname==='/' && !document.querySelector('[data-home-transition]')",
			"切回首页后展开和收起侧栏",
		);
		const homeSelectors = [".brand-arcs", ".home-greeting h1", ".composer-shell--home", ".home-footnote"];
		async function sidebarMotion(selector: string, label: string) {
			const before = await browser!.evaluate<number[]>(
				`[${homeSelectors.map((s) => `document.querySelector(${JSON.stringify(s)})`).join(",")}].map(e=>{const r=e.getBoundingClientRect();return r.left+r.width/2;})`,
			);
			await browser!.holdAnimations(".sidebar, .home-main *");
			await browser!.click(selector);
			await browser!.until(
				"window.__heldAnimations.some(a=>a.effect.target.matches('.sidebar'))",
				"捕获侧栏动画开始",
			);
			await browser!.evaluate("window.__sidebarAnimations=window.__heldAnimations");
			async function sample(time: number) {
				return browser!.evaluate<number[]>(
					`(()=>{window.__sidebarAnimations.forEach(a=>a.currentTime=${time});return [${homeSelectors.map((s) => `document.querySelector(${JSON.stringify(s)})`).join(",")}].map(e=>{const r=e.getBoundingClientRect();return r.left+r.width/2;});})()`,
				);
			}
			const start = await sample(0),
				middle = await sample(80),
				end = await sample(450);
			for (let i = 0; i < before.length; i++) {
				assert.ok(
					Math.abs(start[i] - before[i]) < 1,
					`${label} ${homeSelectors[i]} 在首帧保持原位置：${start[i]} / ${before[i]}`,
				);
				assert.ok(
					Math.abs(middle[i] - start[i]) > 0.5 && Math.abs(middle[i] - end[i]) > 0.5,
					`${label} ${homeSelectors[i]} 经过中间位置：${JSON.stringify({ before, start, middle, end, viewport: await browser!.evaluate("innerWidth") })}`,
				);
			}
			await sample(80);
			await browser!.screenshot(join(root, `${label}-middle.png`));
			await browser!.releaseAnimations();
			return { before, start, middle, end };
		}
		const collapse = await sidebarMotion('[aria-label="收起侧栏"]', "home-sidebar-collapse");
		const expand = await sidebarMotion('[aria-label="展开侧栏"]', "home-sidebar-expand");
		async function sidebarCoverage(theme: string) {
			await browser!.click('[aria-label="收起侧栏"]');
			await browser!.evaluate("Promise.all(document.querySelector('.sidebar').getAnimations().map(a=>a.finished))");
			await browser!.holdAnimations(".sidebar, .home-main *");
			await browser!.click('[aria-label="展开侧栏"]');
			await browser!.until("window.__heldAnimations.some(a=>a.effect.target.matches('.sidebar'))", "捕获侧栏展开");
			await browser!.evaluate("window.__openingAnimations=window.__heldAnimations");
			const frames: {
				time: number;
				sidebarRight: number;
				homeLeft: number;
				backgroundLeft: number;
				covered: boolean;
				hit: string;
			}[] = [];
			for (const time of [0, 40, 80, 160, 320]) {
				frames.push(
					await browser!.evaluate(
						`(()=>{window.__openingAnimations.forEach(a=>a.currentTime=${time});const side=document.querySelector('.sidebar').getBoundingClientRect(),home=document.querySelector('.home-main').getBoundingClientRect(),background=document.querySelector('.home-backdrop').getBoundingClientRect();const x=(Math.max(side.right,0)+home.left)/2;const hit=document.elementFromPoint(x,100);return {time:${time},sidebarRight:side.right,homeLeft:home.left,backgroundLeft:background.left,covered:side.right>=home.left-.5||!!hit?.closest('.home-backdrop'),hit:hit?.className??''};})()`,
					),
				);
			}
			await browser!.evaluate("window.__openingAnimations.forEach(a=>a.currentTime=80)");
			await browser!.screenshot(join(root, `sidebar-${theme}-opening-80ms.png`));
			await writeFile(join(root, `sidebar-${theme}-coverage.json`), JSON.stringify(frames, null, 2));
			assert.ok(
				frames.every((frame) => frame.covered),
				`${theme} 侧栏展开时蓝白背景持续覆盖未被侧栏占据的区域：${JSON.stringify(frames)}`,
			);
			await browser!.releaseAnimations();
			await browser!.until("!document.querySelector('[data-sidebar-reveal]')", `${theme} 背景越界绘制状态清理`);
			return frames;
		}
		const lightOpeningCoverage = await sidebarCoverage("light");
		// 在动画中途反向，新的首帧必须衔接当前可见位置。
		await browser.holdAnimations(".sidebar, .home-main *");
		await browser.click('[aria-label="收起侧栏"]');
		await browser.until("document.querySelector('.home-center').getAnimations().length>0", "中途反向前的布局动画");
		await browser.evaluate("window.__heldAnimations.forEach(a=>a.currentTime=80)");
		const reversal = await sidebarMotion('[aria-label="展开侧栏"]', "home-sidebar-reversal");
		await browser.click(".sidebar-account-trigger");
		await browser.until("!!document.querySelector('[aria-label^=\"主题，当前\"]')", "主题菜单");
		await browser.click('[aria-label^="主题，当前"]');
		await browser.until("document.documentElement.dataset.theme==='dark'", "深色主题");
		await browser.call("Input.dispatchKeyEvent", { type: "keyDown", key: "Escape", code: "Escape" });
		await browser.call("Input.dispatchKeyEvent", { type: "keyUp", key: "Escape", code: "Escape" });
		const darkRoot = join(root, "dark");
		await mkdir(darkRoot, { recursive: true });
		const darkWorkspace = await transition(browser, `.sidebar-session[href="/s/${id}"]`, "to-workspace", darkRoot);
		const darkHome = await transition(browser, ".sidebar-new", "to-home", darkRoot);
		assert.ok(
			darkHome.middle["workspace-composer"].y > 0 && darkWorkspace.start["workspace-composer"].opacity < 0.01,
			JSON.stringify({ darkHome, darkWorkspace }),
		);
		const darkOpeningCoverage = await sidebarCoverage("dark");
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 800,
			height: 900,
			deviceScaleFactor: 1,
			mobile: false,
		});
		const narrowRoot = join(root, "narrow");
		await mkdir(narrowRoot, { recursive: true });
		const narrowWorkspace = await transition(
			browser,
			`.sidebar-session[href="/s/${id}"]`,
			"to-workspace",
			narrowRoot,
		);
		const narrowHome = await transition(browser, ".sidebar-new", "to-home", narrowRoot);
		assert.ok(
			narrowWorkspace.middle["home-curtain-left"].x < 0 &&
				narrowWorkspace.middle["home-curtain-right"].x > 0 &&
				narrowHome.start["home-composer"].y > 0,
			JSON.stringify({ narrowWorkspace, narrowHome }),
		);
		assert.equal(
			await browser.evaluate("document.documentElement.scrollWidth<=innerWidth"),
			true,
			"窄屏动效不造成横向溢出",
		);
		await browser.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "reduce" }],
		});
		await browser.click('[aria-label="展开侧栏"]');
		await browser.click(`.sidebar-session[href="/s/${id}"]`);
		await browser.evaluate("window.__latestMotionTransition.finished");
		await browser.until(
			"location.pathname.startsWith('/s/') && !document.querySelector('[data-home-transition]')",
			"减少动态仍正常导航",
		);
		assert.equal(
			await browser.evaluate(
				"document.getAnimations().filter(a=>a.effect?.pseudoElement||a.effect?.target?.closest?.('.home-main')).length",
			),
			0,
		);
		await browser.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "no-preference" }],
		});
		await browser.call("Page.addScriptToEvaluateOnNewDocument", {
			source: "document.startViewTransition=undefined;",
		});
		await browser.navigate(`http://127.0.0.1:${port}/`);
		await browser.until("!!document.querySelector('.sidebar-session')", "降级浏览器也能读取会话");
		if (await browser.evaluate("document.querySelector('.sidebar')?.inert")) {
			await browser.click('[aria-label="展开侧栏"]');
			await browser.evaluate("Promise.all(document.querySelector('.sidebar').getAnimations().map(a=>a.finished))");
		}
		await browser.click(`.sidebar-session[href="/s/${id}"]`);
		await browser.until("!!document.querySelector('.chat-composer-area')", "无原生过渡时工作台可用");
		const fallback = await browser.evaluate<{
			supported: boolean;
			count: number;
			start: { opacity: number; y: number };
			end: { opacity: number; transform: string };
		}>(
			"(()=>{const e=document.querySelector('.chat-composer-area');const animations=e.getAnimations();animations.forEach(a=>{a.pause();a.currentTime=0});const before=getComputedStyle(e);const start={opacity:parseFloat(before.opacity),y:new DOMMatrixReadOnly(before.transform).m42};animations.forEach(a=>a.finish());const after=getComputedStyle(e);return {supported:typeof document.startViewTransition==='function',count:animations.length,start,end:{opacity:parseFloat(after.opacity),transform:after.transform}};})()",
		);
		assert.equal(fallback.supported, false);
		assert.ok(fallback.start.y > 0 && fallback.start.opacity < 0.01, "无原生快照也有局部缓入");
		await browser.screenshot(join(root, "fallback-workspace.png"));
		await writeFile(
			join(root, "report.json"),
			JSON.stringify(
				{
					intoHome,
					intoWorkspace,
					collapse,
					expand,
					reversal,
					lightOpeningCoverage,
					darkOpeningCoverage,
					darkHome,
					darkWorkspace,
					narrowHome,
					narrowWorkspace,
					reducedMotion: true,
					fallback,
				},
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
