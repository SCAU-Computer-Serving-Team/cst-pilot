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
			const result={};for(const name of ['home-background','home-regions','home-mark','home-greeting','home-composer','home-footer','workspace-composer','workspace-header','workspace-messages']){
				const side=name==='home-regions'?'new':name.startsWith('workspace-')?${JSON.stringify(direction === "to-home" ? "old" : "new")}:${JSON.stringify(direction === "to-home" ? "new" : "old")};
				const s=getComputedStyle(document.documentElement,'::view-transition-'+side+'('+name+')');
				const g=getComputedStyle(document.documentElement,'::view-transition-group('+name+')');
				const timing=animations.find(a=>a.effect.pseudoElement==='::view-transition-'+side+'('+name+')')?.effect.getTiming();
				result[name]={opacity:parseFloat(s.opacity),x:s.transform==='none'?0:new DOMMatrixReadOnly(s.transform).m41,mask:s.maskImage,progress:Number.parseFloat(s.getPropertyValue('--home-exit-progress'))||0,y:s.transform==='none'?0:new DOMMatrixReadOnly(s.transform).m42,blur:s.filter.startsWith('blur(')?parseFloat(s.filter.slice(5)):0,group:g.transform,clipping:g.overflow,zIndex:Number(g.zIndex),duration:Number(timing?.duration),delay:timing?.delay??0};
			}return result;
		}
		const scale=${direction === "to-home" ? 1 : 0.8};
		const start=frame(0),middle=frame(80*scale),late=frame(400*scale),end=frame(600*scale);
		window.__motion={transition:t,animations,frames:{start,middle,late,end}};
	};`);
	await browser.click(selector);
	await browser.until("!!window.__motion", `${direction} 产生原生过渡`);
	const frames = await browser.evaluate<{ start: Frame; middle: Frame; late: Frame; end: Frame }>(
		"window.__motion.frames",
	);
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
		// 自然播放验收：不暂停、不改播放头，检查实际 ready → finished 与播放中的 DOM 状态。
		await browser.evaluate(`(()=>{
			window.__naturalTransitions=[];window.__regionPixels=[];
			const native=document.startViewTransition.bind(document);
			document.startViewTransition=(...args)=>{
				const observation={from:document.querySelector('.home-main')?'home':'workspace',frames:[]};
				const t=native(...args);window.__naturalTransitions.push(observation);
				t.ready.then(()=>{
					observation.to=document.querySelector('.home-main')?'home':'workspace';
					observation.readyAt=performance.now();
					observation.animationResults=[];
					for(const a of document.getAnimations().filter(a=>a.effect?.pseudoElement))a.finished.then(()=>observation.animationResults.push({pseudo:a.effect.pseudoElement,settled:'finished',time:performance.now()-observation.readyAt}),e=>observation.animationResults.push({pseudo:a.effect.pseudoElement,settled:e.name,time:performance.now()-observation.readyAt}));
					function sample(){
						if(observation.finishedAt)return;
						observation.frames.push({time:performance.now()-observation.readyAt,transitioning:!!document.querySelector('[data-home-transition]'),animations:document.getAnimations().filter(a=>a.effect?.pseudoElement).map(a=>({pseudo:a.effect.pseudoElement,state:a.playState,time:a.currentTime,duration:a.effect.getTiming().duration,delay:a.effect.getTiming().delay}))});
						if(observation.to==='workspace'){
							const canvas=document.querySelector('.home-backdrop canvas'),gl=canvas?.getContext('webgl'),program=gl?.getParameter(gl.CURRENT_PROGRAM);
							if(program){
								const uniform=name=>gl.getUniform(program,gl.getUniformLocation(program,name));
								const progress=uniform('u_exit_progress');
								const previous=window.__regionPixels.at(-1);
								if(progress>0.01&&(!previous||progress<previous.progress||progress-previous.progress>=0.04)){
									const width=canvas.width,height=canvas.height,rect=canvas.getBoundingClientRect(),count=uniform('u_count'),travel=(uniform('u_phase')+uniform('u_time')*uniform('u_speed')/100*1.2)*0.48;
									const row=new Uint8Array(width*4);gl.readPixels(0,Math.floor(height/2),width,1,gl.RGBA,gl.UNSIGNED_BYTE,row);
									const readAlpha=x=>row[Math.max(0,Math.min(width-1,Math.floor(x*width)))*4+3]/255;
									const alpha=[.005,.08,.18,.3,.4,.5,.6,.7,.82,.92,.995].map(readAlpha);
									const bands=[];for(let lane=Math.floor(-travel);lane<Math.ceil(count-travel);lane++){const inner=Math.max(0,(lane+travel)/count),outer=Math.min(1,(lane+1+travel)/count),radius=(inner+outer)/2;bands.push({lane,inner,outer,left:readAlpha((1-radius)/2),right:readAlpha((1+radius)/2)});}
									const rowAlpha=Array.from({length:101},(_,i)=>readAlpha(i/100));
									window.__regionPixels.push({transition:window.__naturalTransitions.length-1,progress,travel,origin:uniform('u_exit_origin'),width:rect.width,left:rect.left,transform:getComputedStyle(canvas.parentElement).transform,alpha,count,bands,rowAlpha});
								}
							}
						}
						requestAnimationFrame(sample);
					}sample();
				});
				t.finished.then(()=>{observation.finishedAt=performance.now();observation.elapsed=observation.finishedAt-observation.readyAt;});
				return t;
			};
		})()`);
		await browser.click(".sidebar-new");
		await browser.until("window.__naturalTransitions[0]?.finishedAt>0", "返回主页自然播放完成");
		await browser.click(`.sidebar-session[href="/s/${id}"]`);
		await browser.until(
			"window.__regionPixels.some(p=>p.transition===1&&p.progress>=.32)",
			"中心区域淡出，外侧仍可见",
		);
		await browser.screenshot(join(root, "region-fade-desktop-live.png"));
		await browser.until("window.__naturalTransitions[1]?.finishedAt>0", "进入工作区自然播放完成");
		await browser.click(".product-name");
		await browser.until("window.__naturalTransitions[2]?.finishedAt>0", "品牌入口返回主页自然播放完成");
		await browser.fill(".composer-input", "主页发送时的方向验收");
		await browser.click(".composer-send");
		await browser.until(
			"window.__regionPixels.some(p=>p.transition===3&&p.progress>=.32)",
			"主页发送后的工作区按区域显露",
		);
		await browser.screenshot(join(root, "region-fade-content-live.png"));
		await browser.until("window.__naturalTransitions[3]?.finishedAt>0", "主页发送进入工作区自然播放完成");
		const sentPath = await browser.evaluate<string>("location.pathname");
		const natural =
			await browser.evaluate<
				{
					from: string;
					to: string;
					elapsed: number;
					animationResults: { settled: string }[];
					frames: { time: number; transitioning: boolean; animations: { pseudo: string; duration: number }[] }[];
				}[]
			>("window.__naturalTransitions");
		const pixels =
			await browser.evaluate<
				{
					transition: number;
					progress: number;
					travel: number;
					origin: number;
					width: number;
					left: number;
					transform: string;
					alpha: number[];
					rowAlpha: number[];
					count: number;
					bands: { lane: number; inner: number; outer: number; left: number; right: number }[];
				}[]
			>("window.__regionPixels");
		await writeFile(join(root, "region-fade-pixels.json"), JSON.stringify(pixels, null, 2));
		const first = pixels.filter((p) => p.transition === 1);
		// 不以云端软件WebGL的帧数判定动效；实际像素必须覆盖起始、中间与末段。
		assert.ok(
			first.some((p) => p.progress < 0.3) &&
				first.some((p) => p.progress >= 0.3 && p.progress < 0.85) &&
				first.some((p) => p.progress >= 0.85),
			"实际GPU采样覆盖完整退场时间阶段",
		);
		assert.ok(
			first.some((p) => p.progress > 0.2 && p.progress < 0.85 && p.alpha[0] - p.alpha[5] > 0.6),
			"中心透明度先于外侧降低",
		);
		assert.ok(
			first.some((p) => p.alpha[5] < 0.05 && p.alpha[0] > 0.1),
			"中心先消失时，外侧尚未消失",
		);
		assert.ok(first.at(-1)!.travel > first[0].travel, "分区边缘随背景时间实时变化");
		for (const sample of first) {
			for (let x = 1; x < sample.rowAlpha.length; x++)
				assert.ok(Math.abs(sample.rowAlpha[x] - sample.rowAlpha[x - 1]) < 0.06, "实际GPU透明度跨区域平滑叠加");
			assert.equal(sample.origin, first[0].origin, "区域的淡出编号在转场内保持连续");
			for (const band of sample.bands) {
				if (((band.outer - band.inner) * sample.width) / 2 < 14) continue;
				const delay = (Math.max(0, Math.min(sample.count, band.lane + sample.origin)) / sample.count) * 0.45;
				const t = Math.max(0, Math.min(1, (sample.progress - delay) / 0.55));
				const radius = (band.inner + band.outer) / 2;
				const wave = Math.max(0, Math.min(1, (sample.progress - radius * 0.45) / 0.55));
				const expected = (1 - wave * wave * (3 - 2 * wave)) * 0.8 + (1 - t * t * (3 - 2 * t)) * 0.2;
				assert.ok(
					Math.abs(band.left - expected) < 0.015 && Math.abs(band.right - expected) < 0.015,
					"GPU区域透明度与背景自身色带边缘及顺序一致",
				);
			}
		}
		assert.ok(
			first.some((p) => p.progress > 0.85 && p.alpha[0] < 0.9),
			"外侧在末段淡出",
		);
		for (let i = 1; i < first.length; i++) {
			// 进度到1后，原背景仍可运行到React清理；只核查未完成阶段的线性速度。
			if (first[i].progress < 1)
				assert.ok(
					Math.abs(
						first[i].travel -
							first[i - 1].travel -
							0.4 * 0.36 * 0.48 * (first[i].progress - first[i - 1].progress),
					) < 0.002,
					"400ms内边缘沿原背景速度连续移动",
				);
			assert.equal(first[i].left, first[0].left, "背景矩形不平移");
			assert.equal(first[i].width, first[0].width, "背景矩形不拉伸");
			assert.equal(first[i].transform, "none");
			for (let j = 0; j < first[i].alpha.length; j++) {
				assert.ok(first[i].alpha[j] <= first[i - 1].alpha[j] + 0.012, "已淡出的像素不重新显露");
				assert.ok(
					first[i - 1].alpha[j] - first[i].alpha[j] <= 3.2 * (first[i].progress - first[i - 1].progress) + 0.012,
					"实际GPU相邻采样的透明度变化平滑",
				);
			}
		}
		assert.ok(first.at(-1)!.progress > 0.9 && Math.max(...first.at(-1)!.alpha) < 0.2, "末帧区域接近完全透明");
		await writeFile(join(root, "natural-direction-timing.json"), JSON.stringify(natural, null, 2));
		assert.equal(natural[0].from, "workspace");
		assert.equal(natural[0].to, "home");
		assert.ok(natural[0].elapsed >= 450 && natural[0].elapsed < 1000, `返回主页使用快速时间：${natural[0].elapsed}`);
		assert.ok(
			natural[0].frames.some(
				(f) =>
					f.time >= 200 &&
					f.transitioning &&
					f.animations.some((a) => a.pseudo === "::view-transition-new(home-background)"),
			),
			"200ms后主页背景仍在播放",
		);
		assert.equal(natural[1].from, "home");
		assert.equal(natural[1].to, "workspace");
		assert.ok(
			natural[1].frames.some(
				(f) =>
					f.time >= 200 &&
					f.transitioning &&
					f.animations.some((a) => a.pseudo === "::view-transition-new(workspace-messages)"),
			),
			"200ms后会话内容快照仍在播放",
		);
		assert.ok(natural[1].elapsed >= 350, `进入工作区完整播放：${natural[1].elapsed}`);
		assert.ok(natural[1].elapsed < 900, `进入工作区缩短时长：${natural[1].elapsed}`);
		for (const [index, to, duration] of [
			[2, "home", 500],
			[3, "workspace", 320],
		] as const) {
			assert.equal(natural[index].to, to);
			assert.ok(natural[index].elapsed >= duration * 0.9, "品牌入口与主页发送的过渡完整播放");
			assert.ok(
				natural[index].frames.some((f) =>
					f.animations.some(
						(a) =>
							a.pseudo ===
								`::view-transition-new(${to === "home" ? "home-background" : "workspace-composer"})` &&
							Math.round(a.duration) === duration,
					),
				),
				"不同入口使用正确方向的时长",
			);
		}
		assert.ok(
			natural.every((t) => t.animationResults.every((a) => a.settled === "finished")),
			"自然播放中的动画全部正常结束",
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
		assert.equal(intoHome.start["workspace-composer"].duration, 350);
		assert.equal(intoHome.start["workspace-header"].duration, 250);
		assert.equal(intoHome.start["workspace-messages"].duration, 250);
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
		const regions = intoWorkspace.middle["home-regions"];
		for (const name of ["workspace-header", "workspace-messages", "workspace-composer"]) {
			assert.ok(regions.zIndex > intoWorkspace.middle[name].zIndex, `背景覆盖${name}`);
		}
		assert.equal(intoWorkspace.start["home-regions"].duration, 400);
		assert.equal(regions.x, 0, "区域不做横向slide");
		assert.equal(regions.y, 0, "区域不做纵向slide");
		assert.equal(regions.group, intoWorkspace.start["home-regions"].group, "背景矩形固定");
		assert.ok(regions.progress > 0 && regions.progress < intoWorkspace.late["home-regions"].progress);
		assert.equal(intoWorkspace.end["home-regions"].progress, 1);
		assert.equal(regions.opacity, 1, "不进行整体淡出，透明度由各色带控制");
		assert.equal(regions.clipping, "hidden");
		assert.equal(intoWorkspace.middle["home-mark"].y, 0, "LOGO原位淡出");
		assert.equal(intoWorkspace.middle["home-greeting"].y, 0, "大字原位淡出");
		assert.equal(Math.round(intoWorkspace.start["workspace-composer"].duration), 320, "进入工作区输入区同步加快");
		assert.equal(Math.round(intoWorkspace.start["home-composer"].duration * 10) / 10, 280, "主页输入框退场同步加快");
		assert.equal(Math.round(intoWorkspace.start["home-mark"].duration * 10) / 10, 280, "主页LOGO退场同步加快");
		assert.equal(intoWorkspace.start["home-greeting"].duration, 200, "主页大字退场同步加快");
		assert.equal(intoWorkspace.start["workspace-header"].blur, 1, "前向顶栏模糊降低");
		assert.equal(intoWorkspace.start["workspace-composer"].blur, 1, "前向输入框模糊降低");
		assert.equal(intoWorkspace.start["workspace-messages"].blur, 1.5, "前向正文模糊降低");
		assert.equal(intoWorkspace.end["home-composer"].blur, 1, "前向退场输入框模糊降低");
		assert.equal(regions.blur, 0, "背景整体不添加模糊");
		assert.equal(intoHome.start["home-composer"].blur, 2, "返回主页模糊保持");
		assert.ok(intoWorkspace.middle["home-composer"].zIndex > regions.zIndex, "主页退场输入框仍在自身背景上方");
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
			narrowWorkspace.middle["home-regions"].x === 0 &&
				narrowWorkspace.middle["home-regions"].progress > 0 &&
				narrowHome.start["home-composer"].y > 0,
			JSON.stringify({ narrowWorkspace, narrowHome }),
		);
		assert.equal(
			await browser.evaluate("document.documentElement.scrollWidth<=innerWidth"),
			true,
			"窄屏动效不造成横向溢出",
		);
		for (const frames of [darkWorkspace, narrowWorkspace]) {
			for (const name of ["workspace-header", "workspace-messages", "workspace-composer"])
				assert.ok(
					frames.middle["home-regions"].zIndex > frames.middle[name].zIndex,
					"深色、窄屏背景覆盖工作区组件",
				);
			assert.equal(frames.start["home-regions"].duration, 400);
		}
		if (await browser.evaluate("document.querySelector('.sidebar')?.inert"))
			await browser.click('[aria-label="展开侧栏"]');
		await browser.click(`.sidebar-session[href="${sentPath}"]`);
		await browser.until(
			"!!document.querySelector('[data-home-exit]')&&Number.parseFloat(getComputedStyle(document.documentElement,'::view-transition-new(home-regions)').getPropertyValue('--home-exit-progress'))>.32",
			"深色窄屏实际正文显露",
		);
		await browser.screenshot(join(root, "region-fade-content-narrow-dark-live.png"));
		await browser.evaluate("window.__latestMotionTransition.finished");
		if (await browser.evaluate("document.querySelector('.sidebar')?.inert"))
			await browser.click('[aria-label="展开侧栏"]');
		await browser.click(".sidebar-new");
		await browser.until(
			"location.pathname==='/'&&!document.querySelector('[data-home-transition]')",
			"深色窄屏返回主页",
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
			"(()=>{const native=document.startViewTransition.bind(document);document.startViewTransition=(...args)=>{const t=native(...args);window.__cssTransition=t;t.ready.then(()=>{function sample(){const e=document.querySelector('.home-backdrop'),progress=Number.parseFloat(getComputedStyle(document.documentElement,'::view-transition-new(home-regions)').getPropertyValue('--home-exit-progress'));if(!window.__cssRegions&&e&&progress>=.55&&progress<=.75&&e.style.maskImage.includes('gradient'))window.__cssRegions={mask:e.style.maskImage,transform:getComputedStyle(e).transform,renderer:e.querySelector('canvas').dataset.renderer,progress};if(progress<1&&document.querySelector('[data-home-transition]'))requestAnimationFrame(sample);}sample();});return t;};})()",
		);
		await browser.click(".sidebar-new");
		await browser.until("location.pathname==='/'&&!!window.__cssTransition", "无WebGL主页入场");
		await browser.evaluate("window.__cssTransition.finished");
		await browser.click(`.sidebar-session[href="/s/${id}"]`);
		await browser.until("!!window.__cssRegions", "无WebGL按区域淡出");
		const cssRegions = await browser.evaluate<{ mask: string; transform: string; renderer: string | undefined }>(
			"window.__cssRegions",
		);
		assert.ok(cssRegions.mask.includes("rgba(0, 0, 0, 0)"), "降级中心区域已透明");
		assert.ok(
			[...cssRegions.mask.matchAll(/rgba\(0,\s*0,\s*0,\s*([\d.]+)\)/g)].some((m) => Number(m[1]) > 0.4),
			"降级外侧区域尚未消失",
		);
		assert.equal(cssRegions.transform, "none");
		assert.equal(cssRegions.renderer, undefined);
		await browser.screenshot(join(root, "region-fade-css-live.png"));
		await browser.evaluate("window.__cssTransition.finished");
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
					cssRegions,
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
