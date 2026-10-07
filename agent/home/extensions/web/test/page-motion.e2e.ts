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
	progress: number;
	blur: number;
	group: string;
	clipping: string;
	zIndex: number;
	duration: number;
	easing: string;
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
        animations.forEach(a=>a.pause());
		function frame(time){
			animations.forEach(a=>a.currentTime=time);
			const result={};for(const name of ['home-background','home-mark','home-greeting','home-composer','home-footer','workspace-composer','workspace-header','workspace-messages']){
				const side=name.startsWith('workspace-')?${JSON.stringify(direction === "to-home" ? "old" : "new")}:${JSON.stringify(direction === "to-home" ? "new" : "old")};
				const s=getComputedStyle(document.documentElement,'::view-transition-'+side+'('+name+')');
				const g=getComputedStyle(document.documentElement,'::view-transition-group('+name+')');
				const effect=animations.find(a=>a.effect.pseudoElement==='::view-transition-'+side+'('+name+')')?.effect;
				const timing=effect?.getTiming();
				const easing=effect?.getKeyframes().find(frame=>frame.offset===0)?.easing??timing?.easing;
				result[name]={opacity:parseFloat(s.opacity),x:s.transform==='none'?0:new DOMMatrixReadOnly(s.transform).m41,mask:s.maskImage,progress:Number.parseFloat(s.getPropertyValue('--home-exit-progress'))||0,y:s.transform==='none'?0:new DOMMatrixReadOnly(s.transform).m42,blur:s.filter.startsWith('blur(')?parseFloat(s.filter.slice(5)):0,group:g.transform,clipping:g.overflow,zIndex:Number(g.zIndex),duration:Number(timing?.duration),easing,delay:timing?.delay??0};
			}return result;
		}
		const start=frame(0),middle=frame(80),afterExit=frame(150),beforeHandoff=frame(200),late=frame(320),end=frame(400);
		window.__motion={transition:t,animations,frames:{start,middle,afterExit,beforeHandoff,late,end}};
	};`);
	await browser.click(selector);
	await browser.until("!!window.__motion", `${direction} 产生原生过渡`);
	const frames = await browser.evaluate<{
		start: Frame;
		middle: Frame;
		afterExit: Frame;
		beforeHandoff: Frame;
		late: Frame;
		end: Frame;
	}>("window.__motion.frames");
	if (!root.endsWith("dark") && !root.endsWith("narrow")) {
		for (const time of [80, 250, 400]) {
			await browser.evaluate(`window.__motion.animations.forEach(a=>a.currentTime=${time})`);
			await browser.screenshot(join(root, `${direction}-${time}ms.png`));
		}
	}
	await browser.evaluate("window.__motion.animations.forEach(a=>a.finish())");
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
		const slot = pool.get(id);
		assert.ok(slot);
		const user = {
			role: "user" as const,
			content: [{ type: "text" as const, text: "工作区文字层核查" }],
			timestamp: Date.now(),
		};
		const reply = {
			role: "assistant" as const,
			content: [
				{
					type: "text" as const,
					text: "### 工作区旧文字\n\n蓝白背景滑入时，这段文字应当已退出，并始终位于背景快照下方。\n\n- 第一条诊断内容\n- 第二条诊断内容\n\n**不可与主页欢迎语混在同一层显示。**",
				},
			],
			api: "openai-completions" as const,
			provider: "probe",
			model: "one",
			stopReason: "stop" as const,
			usage: {
				input: 0,
				output: 0,
				cacheRead: 0,
				cacheWrite: 0,
				totalTokens: 0,
				cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
			},
			timestamp: Date.now(),
		};
		slot.session.sessionManager.appendMessage(user);
		slot.session.sessionManager.appendMessage(reply);
		slot.session.agent.state.messages = [user, reply];
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
			`(()=>{window.__naturalTransitions=[];const native=document.startViewTransition.bind(document);document.startViewTransition=(...args)=>{const t=native(...args),v={from:document.querySelector('.home-main')?'home':'workspace',animationResults:[]};window.__naturalTransitions.push(v);t.ready.then(()=>{v.readyAt=performance.now();v.to=document.querySelector('.home-main')?'home':'workspace';for(const a of document.getAnimations().filter(a=>a.effect?.pseudoElement))a.finished.then(()=>v.animationResults.push('finished'),e=>v.animationResults.push(e.name));});t.finished.then(()=>{v.finishedAt=performance.now();v.elapsed=v.finishedAt-v.readyAt;});return t;};})()`,
		);
		for (const selector of [".sidebar-new", `.sidebar-session[href="/s/${id}"]`, ".product-name"]) {
			await browser.click(selector);
			await browser.until(
				"window.__naturalTransitions.at(-1)?.finishedAt>0&&!document.querySelector('[data-home-transition]')",
				"自然slide完整结束",
			);
		}
		await browser.fill(".composer-input", "主页发送时的方向验收");
		await browser.click(".composer-send");
		await browser.until(
			"window.__naturalTransitions.length===4&&window.__naturalTransitions[3]?.finishedAt>0",
			"真实发送slide完整结束",
		);
		const natural =
			await browser.evaluate<{ from: string; to: string; elapsed: number; animationResults: string[] }[]>(
				"window.__naturalTransitions",
			);
		assert.ok(
			natural.every((t) => t.elapsed >= 350 && t.animationResults.every((a) => a === "finished")),
			"所有入口自然播放，加载容器替换不取消快照",
		);
		await writeFile(join(root, "natural-direction-timing.json"), JSON.stringify(natural, null, 2));
		await browser.navigate(`http://127.0.0.1:${port}/s/${id}`);
		await browser.until(
			"document.querySelector('.conversation')?.textContent.includes('工作区旧文字')",
			"带正文的工作区用于层级核查",
		);
		await browser.evaluate(
			`(()=>{const native=document.startViewTransition.bind(document);document.startViewTransition=(...args)=>{const t=native(...args);window.__latestMotionTransition=t;if(window.__pauseNext){window.__pauseNext=false;t.ready.then(()=>window.__captureMotion(t));}return t;};})()`,
		);
		const intoHome = await transition(browser, ".sidebar-new", "to-home", root);
		assert.equal(intoHome.start["home-background"].duration, 250);
		assert.equal(intoHome.start["home-background"].delay, 150, "先退出工作区白条和输入框，再滑入背景");
		for (const name of ["home-mark", "home-greeting", "home-composer", "home-footer"]) {
			assert.equal(intoHome.start[name].duration, 250);
			assert.equal(intoHome.start[name].delay, 150);
		}
		for (const name of ["workspace-composer", "workspace-header", "workspace-messages"]) {
			assert.equal(intoHome.start[name].duration, 150);
			assert.equal(intoHome.start[name].delay, 0);
			assert.ok(intoHome.start["home-background"].zIndex > intoHome.start[name].zIndex, `背景快照覆盖旧${name}`);
			assert.equal(intoHome.afterExit[name].opacity, 0, "150ms旧工作区完整退出");
		}
		assert.ok(
			intoHome.start["home-composer"].zIndex > intoHome.start["home-background"].zIndex,
			"主页输入框在自己的背景上方",
		);
		assert.equal(intoHome.middle["home-background"].opacity, 0, "旧工作区退出前背景保持未入场");
		assert.ok(
			intoHome.start["home-composer"].y > 0 && intoHome.start["home-composer"].opacity < 0.01,
			"首页聊天框从终点下方淡入",
		);
		assert.ok(intoHome.beforeHandoff["home-composer"].opacity > 0, "主页聊天框从150ms开始进入，不拖到尾段");
		assert.ok(intoHome.middle["workspace-composer"].y > 0, "工作区底部框立即向下退出");
		assert.ok(intoHome.middle["workspace-header"].y < 0, "工作区白条立即向上退出");
		assert.ok(
			intoHome.late["home-composer"].y < intoHome.start["home-composer"].y &&
				intoHome.late["home-composer"].opacity > 0,
		);
		assert.ok(
			intoHome.late["workspace-composer"].y > 0 && intoHome.late["workspace-composer"].opacity < 1,
			"工作台聊天框在背景进入前完整退出",
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
		for (const [direction, frame] of [
			["to-home", intoHome.start],
			["to-workspace", intoWorkspace.start],
		] as const) {
			for (const [name, snapshot] of Object.entries(frame)) {
				const homeExit = direction === "to-workspace" && name.startsWith("home-");
				assert.equal(
					snapshot.easing,
					homeExit ? "ease-in-out" : "cubic-bezier(0.22, 1, 0.36, 1)",
					`${direction} ${name}曲线`,
				);
			}
		}
		await writeFile(
			join(root, "curve-timing.json"),
			JSON.stringify({ intoHome: intoHome.start, intoWorkspace: intoWorkspace.start }, null, 2),
		);
		const bg = intoWorkspace.middle["home-background"];
		for (const [direction, frames] of [
			["to-home", intoHome],
			["to-workspace", intoWorkspace],
		] as const) {
			assert.equal(
				Math.max(...Object.values(frames.start).map((s) => s.duration + s.delay)),
				400,
				`${direction}包含延迟总400ms`,
			);
			for (const name of ["workspace-header", "workspace-composer", "workspace-messages"]) {
				assert.equal(frames.start[name].delay, direction === "to-home" ? 0 : 150);
				assert.equal(frames.start[name].duration, direction === "to-home" ? 150 : 250);
				if (direction === "to-workspace")
					assert.ok(frames.beforeHandoff[name].opacity > 0.2, "背景滑出末段已出现工作区控件");
			}
		}
		assert.equal(intoWorkspace.afterExit["home-composer"].opacity, 0, "主页聊天框150ms内退完，不叠到工作区");
		assert.equal(intoWorkspace.start["home-composer"].delay, 0);
		assert.equal(intoWorkspace.start["home-composer"].duration, 150);
		assert.ok(intoWorkspace.beforeHandoff["workspace-header"].opacity > 0.2);
		assert.ok(intoWorkspace.beforeHandoff["workspace-composer"].opacity > 0.2);
		assert.equal(intoWorkspace.end["home-background"].opacity, 1, "背景保持实色，通过滑出结束");
		assert.ok(intoWorkspace.end["home-background"].y < bg.y);
		assert.equal(intoWorkspace.start["home-background"].duration, 250);
		assert.ok(bg.y < 0 && bg.opacity === 1, "背景保持实色向上slide退出");
		assert.ok(
			intoHome.start["home-background"].y < bg.y && intoHome.end["home-background"].y === 0,
			"背景从相同方向反向进入",
		);
		assert.ok(
			intoWorkspace.middle["home-mark"].y < 0 && intoWorkspace.middle["home-greeting"].y < 0,
			"弧线与欢迎语反向上移",
		);
		assert.ok(
			intoWorkspace.late["home-composer"].y > 0 && intoWorkspace.start["workspace-composer"].y > 0,
			"两个输入框各自原位下方入退场",
		);
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
			await browser!.holdAnimations(".sidebar, .home-main *, .home-backdrop");
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
			await browser!.holdAnimations(".sidebar, .home-main *, .home-backdrop");
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
						`(()=>{window.__openingAnimations.forEach(a=>a.currentTime=${time});const side=document.querySelector('.sidebar').getBoundingClientRect(),home=document.querySelector('.home-main').getBoundingClientRect(),background=document.querySelector('.home-backdrop').getBoundingClientRect();const x=(Math.max(side.right,0)+home.left)/2,y=100;let covered=x>=background.left&&x<=background.right&&y>=background.top&&y<=background.bottom;for(let e=document.querySelector('.home-backdrop').parentElement;e&&covered;e=e.parentElement){const s=getComputedStyle(e),r=e.getBoundingClientRect();if(s.overflowX!=='visible'&&(x<r.left||x>r.right)||s.overflowY!=='visible'&&(y<r.top||y>r.bottom)||s.visibility==='hidden'||Number(s.opacity)===0)covered=false;}const hit=document.elementFromPoint(x,y);return {time:${time},sidebarRight:side.right,homeLeft:home.left,backgroundLeft:background.left,covered:side.right>=home.left-.5||covered,hit:hit?.className??''};})()`,
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
		await browser.holdAnimations(".sidebar, .home-main *, .home-backdrop");
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
			darkHome.late["workspace-composer"].y > 0 && darkWorkspace.start["workspace-composer"].opacity < 0.01,
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
			narrowWorkspace.middle["home-background"].y < 0 && narrowHome.start["home-background"].y < 0,
			"浅深窄屏双向slide",
		);
		assert.equal(await browser.evaluate("document.documentElement.scrollWidth<=innerWidth"), true, "窄屏不溢出");
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
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 1600,
			height: 1000,
			deviceScaleFactor: 1,
			mobile: false,
		});
		const noGlScript = await browser.call<{ identifier: string }>("Page.addScriptToEvaluateOnNewDocument", {
			source:
				"const getContext=HTMLCanvasElement.prototype.getContext;HTMLCanvasElement.prototype.getContext=function(type,...args){return type==='webgl'||type==='webgl2'?null:getContext.call(this,type,...args);};",
		});
		await browser.navigate(`http://127.0.0.1:${port}/s/${id}`);
		await browser.until(
			"!!document.querySelector('.sidebar-new')&&!!document.querySelector('.composer-input')",
			"无WebGL工作区加载",
		);
		if (await browser.evaluate("document.querySelector('.sidebar').inert"))
			await browser.click('[aria-label="展开侧栏"]');
		await browser.evaluate(
			"(()=>{const native=document.startViewTransition.bind(document);document.startViewTransition=(...args)=>{const t=native(...args);window.__cssTransition=t;return t;};})()",
		);
		await browser.click(".sidebar-new");
		await browser.until("!!window.__cssTransition&&location.pathname==='/'", "无WebGL主页slide启动");
		await browser.evaluate("window.__cssTransition.finished");
		await browser.until("!document.querySelector('[data-home-transition]')", "无WebGL主页slide清理");
		assert.equal(
			await browser.evaluate("document.querySelector('.home-backdrop canvas').dataset.renderer"),
			undefined,
		);
		await browser.evaluate("window.__cssTransition=null");
		await browser.click(`.sidebar-session[href="/s/${id}"]`);
		await browser.until("!!window.__cssTransition", "无WebGL工作区slide启动");
		await browser.evaluate("window.__cssTransition.finished");
		const fallbackBackground = { slide: true, webgl: false };
		await browser.call("Page.removeScriptToEvaluateOnNewDocument", { identifier: noGlScript.identifier });
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
					fallbackBackground,
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
