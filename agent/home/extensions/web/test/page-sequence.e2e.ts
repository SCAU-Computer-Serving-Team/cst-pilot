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

test("主页到工作区序列：背景和控件连续衔接，无全白空档", { timeout: 90000 }, async () => {
	const root = join(await testRoot(), "page-sequence", process.env.CST_PAGE_SEQUENCE_LABEL ?? "current");
	await mkdir(root, { recursive: true });
	const home = await mkdtemp(join(root, "home-"));
	await writeFile(join(home, "settings.json"), JSON.stringify({ defaultProvider: "probe", defaultModel: "one" }));
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
							name: "序列验收模型",
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
	const port = await freePort(),
		origin = `http://127.0.0.1:${port}`,
		api = createWebApi(pool, home, port);
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions: pool.snapshot(), stage: "sequence" }),
		api,
	);
	await listenWebServer(server, port);
	let browser: BrowserProbe | undefined;
	const imageWrites: Promise<void>[] = [];
	const frameErrors: string[] = [];
	try {
		const slot = await pool.create();
		slot.session.setSessionName("新对话");
		browser = await BrowserProbe.launch(root);
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 1600,
			height: 1000,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await browser.navigate(`${origin}/`);
		await browser.until(`!!document.querySelector('.sidebar-session[href="/s/${slot.id}"]')`, "会话入口就绪");
		await browser.evaluate(
			`(()=>{window.__holdSequence=false;const native=document.startViewTransition.bind(document);document.startViewTransition=(...args)=>{const transition=native(...args),sequence={transition,frames:[],finished:false};window.__sequence=sequence;transition.ready.then(()=>{sequence.ready=performance.now();sequence.epoch=Date.now();sequence.animations=document.getAnimations().filter(a=>a.effect?.pseudoElement);sequence.sample=()=>{const sample={time:performance.now()-sequence.ready};for(const [name,side]of [['home-background','old'],['home-composer','old'],['workspace-header','new'],['workspace-composer','new']]){const s=getComputedStyle(document.documentElement,'::view-transition-'+side+'('+name+')');const g=getComputedStyle(document.documentElement,'::view-transition-group('+name+')');const effect=sequence.animations.find(a=>a.effect?.pseudoElement==='::view-transition-'+side+'('+name+')')?.effect;sample[name]={opacity:parseFloat(s.opacity),y:s.transform==='none'?0:new DOMMatrixReadOnly(s.transform).m42,height:parseFloat(g.height),timing:effect?.getTiming()};}sample.backgroundStrength=sample['home-background'].opacity*Math.max(0,1+sample['home-background'].y/sample['home-background'].height);return sample;};if(window.__holdSequence){sequence.animations.forEach(a=>{a.pause();a.currentTime=0;});}else{const capture=()=>{if(sequence.finished)return;sequence.frames.push(sequence.sample());requestAnimationFrame(capture);};requestAnimationFrame(capture);}});transition.finished.then(()=>{sequence.finished=true;sequence.elapsed=performance.now()-sequence.ready;});return transition;};})()`,
		);
		await mkdir(join(root, "natural"), { recursive: true });
		let recording = true;
		let frameIndex = 0;
		const off = browser.onEvent("Page.screencastFrame", (raw) => {
			const frame = raw as { sessionId: number; data: string; metadata: { timestamp: number } };
			void browser!
				.call("Page.screencastFrameAck", { sessionId: frame.sessionId })
				.catch((error) => frameErrors.push(String(error)));
			if (recording) {
				const i = frameIndex++;
				imageWrites.push(
					writeFile(join(root, "natural", `${String(i).padStart(3, "0")}.png`), Buffer.from(frame.data, "base64")),
				);
				imageWrites.push(
					writeFile(join(root, "natural", `${String(i).padStart(3, "0")}.json`), JSON.stringify(frame.metadata)),
				);
			}
		});
		await browser.call("Page.startScreencast", { format: "png", maxWidth: 1600, maxHeight: 1000, everyNthFrame: 1 });
		await browser.click(`.sidebar-session[href="/s/${slot.id}"]`);
		await browser.until("window.__sequence?.finished", "自然切换完成");
		recording = false;
		await browser.call("Page.stopScreencast");
		off();
		const natural = await browser.evaluate<{ frames: Frame[]; elapsed: number; epoch: number }>(
			"({frames:window.__sequence.frames,elapsed:window.__sequence.elapsed,epoch:window.__sequence.epoch})",
		);
		await Promise.all(imageWrites);
		await browser.click(".sidebar-new");
		await browser.until(
			"window.__sequence?.finished&&!document.querySelector('[data-home-transition]')",
			"返回主页准备时间轴",
		);
		await browser.evaluate("window.__holdSequence=true;window.__sequence=null");
		await browser.click(`.sidebar-session[href="/s/${slot.id}"]`);
		await browser.until("!!window.__sequence?.sample", "时间轴就绪");
		await mkdir(join(root, "timeline"), { recursive: true });
		const timeline: Frame[] = [];
		for (const time of [
			0, 40, 80, 100, 120, 140, 150, 160, 170, 180, 190, 200, 210, 220, 230, 240, 250, 280, 320, 360, 400,
		]) {
			await browser.evaluate(`window.__sequence.animations.forEach(a=>a.currentTime=${time})`);
			const frame = await browser.evaluate<Frame>("window.__sequence.sample()");
			frame.time = time;
			timeline.push(frame);
			await browser.screenshot(join(root, "timeline", `${String(time).padStart(3, "0")}ms.png`));
		}
		await browser.evaluate("window.__sequence.animations.forEach(a=>a.finish())");
		await browser.evaluate("window.__sequence.transition.finished");
		const gap = (frame: Frame) =>
			frame.time >= 0 &&
			frame.backgroundStrength < 0.08 &&
			frame["workspace-header"].opacity < 0.2 &&
			frame["workspace-composer"].opacity < 0.2 &&
			frame["home-composer"].opacity < 0.2;
		const timelineGaps = timeline.filter(gap),
			naturalGaps = natural.frames.filter(gap);
		await writeFile(
			join(root, "report.json"),
			JSON.stringify(
				{
					natural,
					timeline,
					naturalImages: frameIndex,
					frameErrors,
					timelineGaps: timelineGaps.map((f) => f.time),
					naturalGaps: naturalGaps.map((f) => f.time),
					note: "backgroundStrength是实际快照剩余高度乘透明度；同时核对控件透明度与序列图片。自然播放不暂停，时间轴截图冻结同一进度。",
				},
				null,
				2,
			),
		);
		assert.equal(frameErrors.length, 0);
		assert.ok(frameIndex >= 3, "保存真实自然播放图像帧");
		assert.ok(natural.frames.length >= 8, "自然播放采样足够覆盖各阶段");
		assert.deepEqual(timelineGaps, [], `时间轴存在低背景/低控件可见性的空档：${timelineGaps.map((f) => f.time)}`);
		assert.deepEqual(naturalGaps, [], `自然播放空档：${naturalGaps.map((f) => f.time)}`);
	} finally {
		await browser?.close();
		await api.close();
		await pool.close();
		server.closeAllConnections();
		await new Promise<void>((r) => server.close(() => r()));
		await rm(home, { recursive: true, force: true });
	}
});
interface SequenceSnapshot {
	opacity: number;
	y: number;
	height: number;
	timing: unknown;
}
interface Frame {
	time: number;
	backgroundStrength: number;
	"home-background": SequenceSnapshot;
	"home-composer": SequenceSnapshot;
	"workspace-header": SequenceSnapshot;
	"workspace-composer": SequenceSnapshot;
}
