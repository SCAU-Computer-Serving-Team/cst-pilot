import assert from "node:assert/strict";
import { mkdtemp, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

type Motion = {
	pseudo: string;
	duration: number;
	delay: number;
	frames: { opacity?: string; transform?: string; filter?: string }[];
};

type Frame = {
	time: number;
	oldOpacity: number;
	newOpacity: number;
	oldY: number;
	newY: number;
	filter: string;
	sidebar: number[];
};

const captureMotion = `(() => {
	window.pageMotions = [];
	window.exportEntries = [];
	if (sessionStorage.getItem('disableVT') === '1') document.startViewTransition = undefined;
	document.addEventListener('animationstart', event => {
		if (!event.target.matches?.('.export-workspace')) return;
		for (const animation of event.target.getAnimations()) {
			animation.pause(); animation.currentTime = 0;
			window.exportEntries.push({duration:animation.effect.getTiming().duration,frames:animation.effect.getKeyframes()});
		}
	});
	if (!document.startViewTransition) return;
	const original = document.startViewTransition.bind(document);
	document.startViewTransition = (...args) => {
		const transition = original(...args);
		transition.ready.then(() => {
			const motions = document.getAnimations().filter(a=>a.effect?.pseudoElement).map(a => {
				a.pause(); a.currentTime = 0;
				return {pseudo:a.effect.pseudoElement, ...a.effect.getTiming(), frames:a.effect.getKeyframes()};
			});
			window.pageMotions.push({motions, sameSidebar:window.retainedSidebar===document.querySelector('.sidebar')});
		});
		return transition;
	};
})()`;

async function finish(browser: BrowserProbe) {
	await browser.evaluate(
		"document.getAnimations().filter(a=>a.effect?.getTiming().iterations!==Infinity).forEach(a=>a.finish())",
	);
	await browser.until("!document.getAnimations().some(a=>a.effect?.pseudoElement)", "页面转场结束");
}

async function frame(browser: BrowserProbe, time: number): Promise<Frame> {
	return browser.evaluate<Frame>(`(async () => {
		document.getAnimations().filter(a=>a.effect?.pseudoElement).forEach(a=>{a.currentTime=${time}});
		await new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)));
		const old = getComputedStyle(document.documentElement,'::view-transition-old(export-page)');
		const next = getComputedStyle(document.documentElement,'::view-transition-new(export-page)');
		const rect = document.querySelector('.sidebar').getBoundingClientRect();
		return {time:${time},oldOpacity:Number(old.opacity),newOpacity:Number(next.opacity),
			oldY:new DOMMatrix(old.transform).m42,newY:new DOMMatrix(next.transform).m42,
			filter:old.filter+' '+next.filter,sidebar:[rect.x,rect.y,rect.width,rect.height]};
	})()`);
}

test("导出页所有工作台入口、返回和历史导航均完整转场，侧栏静止", { timeout: 180_000 }, async () => {
	const home = await mkdtemp(join(await testRoot(), "export-transition-"));
	const pool = new WebSessionPool({
		cwd: home,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: <T>(load: () => Promise<T>) => load(),
	});
	const slot = await pool.create();
	slot.session.sessionManager.appendMessage({ role: "user", content: "导出页面转场验证", timestamp: Date.now() });
	const other = await pool.create();
	other.session.sessionManager.appendMessage({ role: "user", content: "侧栏切换另一会话", timestamp: Date.now() });
	const port = await freePort();
	const origin = `http://127.0.0.1:${port}`;
	const chatPath = `/s/${slot.id}`;
	const exportPath = `${chatPath}/export`;
	const api = createWebApi(pool, home, port);
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		async () => ({ sessions: await pool.list(), stage: "test" }),
		api,
	);
	const evidence: { label: string; motions: Motion[]; frames: Frame[] }[] = [];
	const screenshots = process.env.CST_CAPTURE_EXPORT_MOTION === "1";
	let browser: BrowserProbe | undefined;
	try {
		await listenWebServer(server, port);
		const page = await BrowserProbe.launch(home);
		browser = page;
		await page.call("Page.addScriptToEvaluateOnNewDocument", { source: captureMotion });
		async function transition(label: string, destination: string, action: () => Promise<unknown>, capture = false) {
			await page.evaluate("window.pageMotions=[]; void(window.retainedSidebar=document.querySelector('.sidebar'))");
			await action();
			await page.until("window.pageMotions.length===1", `${label}入退场快照`, 3000);
			assert.equal(await page.evaluate("location.pathname"), destination, label);
			const result = await page.evaluate<{ motions: Motion[]; sameSidebar: boolean }>("window.pageMotions[0]");
			assert.equal(result.sameSidebar, true, `${label}复用同一侧栏`);
			const oldPage = result.motions.find((motion) => motion.pseudo === "::view-transition-old(export-page)");
			const newPage = result.motions.find((motion) => motion.pseudo === "::view-transition-new(export-page)");
			assert.equal(oldPage?.duration, 80, `${label}短退场`);
			assert.equal(newPage?.duration, 150, `${label}完整入场`);
			assert.equal(newPage?.delay, 80, `${label}退场后入场`);
			assert.equal(oldPage?.frames.at(-1)?.opacity, "0");
			assert.equal(newPage?.frames[0]?.opacity, "0");
			assert.deepEqual(
				result.motions
					.filter((motion) => motion.duration > 0)
					.map((motion) => motion.pseudo)
					.sort(),
				["::view-transition-new(export-page)", "::view-transition-old(export-page)"],
				`${label}只播放右侧整页，无嵌套或侧栏位移`,
			);
			const samples: Frame[] = [];
			for (const time of capture ? [0, 40, 80, 155, 230] : [40, 155]) {
				samples.push(await frame(page, time));
				if (screenshots && capture) await page.screenshot(join(home, `${label}-${time}ms.png`));
			}
			const out = samples.find((sample) => sample.time === 40);
			const incoming = samples.find((sample) => sample.time === 155);
			assert.ok(
				out && out.oldOpacity > 0 && out.oldOpacity < 1 && out.oldY < 0 && out.oldY > -4,
				`${label}实际绘制退场中间帧`,
			);
			assert.equal(out.newOpacity, 0, `${label}入场等待退场完成`);
			assert.ok(
				incoming && incoming.newOpacity > 0 && incoming.newOpacity < 1 && incoming.newY > 0 && incoming.newY < 4,
				`${label}实际绘制入场中间帧`,
			);
			assert.equal(incoming.oldOpacity, 0);
			assert.ok(
				samples.every((sample) => sample.filter === "none none"),
				"转场无模糊",
			);
			assert.ok(
				samples.every((sample) => JSON.stringify(sample.sidebar) === JSON.stringify(samples[0].sidebar)),
				`${label}侧栏逐帧位置不变：${JSON.stringify(samples)}`,
			);
			evidence.push({ label, motions: result.motions, frames: samples });
			await finish(page);
			assert.equal(await page.evaluate("window.exportEntries.length"), 0, "快照结束后不补播局部入场");
			if (screenshots && capture) {
				if (destination === exportPath)
					await page.until(
						"document.querySelector('iframe')?.contentDocument?.documentElement.dataset.exportState==='ready'",
						"导出预览就绪",
					);
				await page.screenshot(join(home, `${label}-finished.png`));
			}
		}
		async function openExport(viaKeyboard = false) {
			if (viaKeyboard) {
				await keyboard(".chat-menu-trigger");
				await page.until(`!!document.querySelector('a[href="${exportPath}"]')`, "键盘打开消息菜单");
				await keyboard(`a[href="${exportPath}"]`);
			} else {
				await page.click(".chat-menu-trigger");
				await page.click(`a[href="${exportPath}"]`);
			}
		}
		async function keyboard(selector: string) {
			await page.evaluate(`document.querySelector(${JSON.stringify(selector)}).focus()`);
			for (const type of ["keyDown", "keyUp"]) {
				await page.call("Input.dispatchKeyEvent", {
					type,
					key: "Enter",
					code: "Enter",
					windowsVirtualKeyCode: 13,
					...(type === "keyDown" ? { text: "\r" } : {}),
				});
			}
		}
		for (const theme of ["light", "dark"]) {
			await fetch(`${origin}/api/settings`, {
				method: "PATCH",
				headers: {
					Origin: origin,
					"X-CST-Web-Request": "1",
					"Content-Type": "application/json",
					"Idempotency-Key": `motion-${theme}`,
				},
				body: JSON.stringify({ theme }),
			});
			for (const width of [1440, 390]) {
				await page.call("Emulation.setDeviceMetricsOverride", {
					width,
					height: 900,
					deviceScaleFactor: 1,
					mobile: false,
				});
				await page.navigate(`${origin}${chatPath}`);
				await page.until("!!document.querySelector('.chat-loaded')", "对话加载");
				if (
					width === 390 &&
					!(await page.evaluate("document.querySelector('.app-layout').classList.contains('sidebar-collapsed')"))
				) {
					await page.click('[aria-label="收起侧栏"]');
				}
				if (
					width === 1440 &&
					(await page.evaluate("document.querySelector('.app-layout').classList.contains('sidebar-collapsed')"))
				) {
					await page.click('[aria-label="展开侧栏"]');
				}
				await finish(page);
				await page.until(`document.documentElement.dataset.theme==='${theme}'`, "主题就绪");
				const name = `${theme}-${width}`;
				await transition(`${name}-enter`, exportPath, () => openExport(width === 390), true);
				assert.equal(
					await page.evaluate("document.documentElement.scrollWidth<=innerWidth && scrollY===0"),
					true,
					"导出页无外层溢出",
				);
				await transition(`${name}-return`, chatPath, () => keyboard(".export-back"), true);
				await transition(`${name}-history-back`, exportPath, () => page.evaluate("history.back()"));
				await transition(`${name}-history-forward`, chatPath, () => page.evaluate("history.forward()"));
			}
		}
		await page.call("Emulation.setDeviceMetricsOverride", {
			width: 1440,
			height: 900,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await page.navigate(`${origin}${chatPath}`);
		if (await page.evaluate("document.querySelector('.app-layout').classList.contains('sidebar-collapsed')"))
			await page.click('[aria-label="展开侧栏"]');
		await finish(page);
		for (const [label, destination, selector] of [
			["sidebar-same", chatPath, `.sidebar-session[data-session-id="${slot.id}"]`],
			["sidebar-other", `/s/${other.id}`, `.sidebar-session[data-session-id="${other.id}"]`],
			["home-brand", "/", ".product-name"],
			["home-new", "/", ".sidebar-new"],
			["sessions", "/sessions", ".sidebar-all"],
			["account", "/account", 'a[href="/account"]'],
			["settings", "/settings", 'a[href="/settings"]'],
		]) {
			await transition(`${label}-enter`, exportPath, openExport);
			await transition(
				`${label}-exit`,
				destination,
				async () => {
					if (label === "account" || label === "settings") await page.click(".sidebar-account-trigger");
					await page.click(selector);
				},
				label === "settings" || label === "home-brand",
			);
			await transition(`${label}-back`, exportPath, () => page.evaluate("history.back()"));
			await transition(`${label}-chat`, chatPath, () => page.click(".export-back"));
		}
		await transition("login-enter-export", exportPath, openExport);
		await page.evaluate("window.pageMotions=[]");
		await page.click(".sidebar-account-trigger");
		await page.click('a[href^="/login?"]');
		await page.until("!!document.querySelector('.login-panel')", "导出页进入登录");
		await page.until("window.pageMotions.length===1", "离开导出至独立登录页的退场", 3000);
		const loginExit = await page.evaluate<Motion[]>("window.pageMotions[0].motions");
		assert.equal(loginExit.find((motion) => motion.pseudo === "::view-transition-old(export-page)")?.duration, 80);
		await finish(page);
		await page.evaluate("window.pageMotions=[]; history.back()");
		await page.until("window.pageMotions.length===1", "从登录历史返回导出页");
		const loginBack = await page.evaluate<Motion[]>("window.pageMotions[0].motions");
		assert.equal(loginBack.find((motion) => motion.pseudo === "::view-transition-new(export-page)")?.duration, 150);
		await finish(page);
		assert.equal(await page.evaluate("window.exportEntries.length"), 0, "从独立路由返回无重复入场");
		await page.navigate(`${origin}${exportPath}`);
		await page.until("window.exportEntries.length===1", "直达导出页入场");
		assert.equal(await page.evaluate("window.exportEntries[0].duration"), 150);
		assert.equal(await page.evaluate("getComputedStyle(document.querySelector('.export-workspace')).opacity"), "0");
		await finish(page);
		// 动画进行中修改系统偏好，应立即呈现最终状态。
		await page.evaluate("window.exportEntries=[]; window.pageMotions=[]");
		await page.click(".export-back");
		await page.until("window.pageMotions.length===1", "切换偏好前的快照");
		await page.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "reduce" }],
		});
		assert.equal((await frame(page, 40)).newY, 0, "减少动态偏好变化时停止位移");
		await finish(page);
		await page.navigate(`${origin}${exportPath}`);
		assert.equal(
			await page.evaluate("document.querySelector('.export-workspace').getAnimations().length"),
			0,
			"减少动态时直达无入场",
		);
		assert.equal(await page.evaluate("getComputedStyle(document.querySelector('.export-workspace')).opacity"), "1");
		await page.click(".export-back");
		await page.until("!!document.querySelector('.chat-loaded')", "减少动态时返回");
		await finish(page);
		await openExport();
		await page.until("!!document.querySelector('.export-workspace')", "减少动态时进入");
		assert.ok(
			await page.evaluate(
				"document.getAnimations().filter(a=>a.effect?.pseudoElement).every(a=>a.effect.getTiming().duration===0)",
			),
			"减少动态无快照位移",
		);
		await finish(page);
		await page.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "no-preference" }],
		});
		await page.evaluate("sessionStorage.setItem('disableVT','1')");
		await page.navigate(`${origin}${chatPath}`);
		await openExport();
		await page.until("window.exportEntries.length===1", "缺少原生快照时导出局部入场");
		assert.equal(await page.evaluate("window.exportEntries[0].duration"), 150);
		await finish(page);
		await page.click(".export-back");
		await page.until("!!document.querySelector('.chat-loaded')", "无快照时返回可用");
		await writeFile(join(home, "motion-evidence.json"), JSON.stringify(evidence, null, 2));
		console.log("EXPORT_TRANSITION_EVIDENCE", home);
	} finally {
		await browser?.close();
		await api.close();
		await pool.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	}
});
