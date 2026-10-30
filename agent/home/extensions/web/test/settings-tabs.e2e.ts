import assert from "node:assert/strict";
import { mkdtemp, writeFile } from "node:fs/promises";
import type { ServerResponse } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

type Slide = {
	path: string;
	direction: string;
	motions: {
		pseudo: string;
		duration: number;
		easing: string;
		fromX: number;
		fromY: number;
		toX: number;
		toY: number;
		frames: { filter?: string }[];
	}[];
};

test("设置四分类按菜单次序左右滑动，下划线同步移动，支持历史、键盘与减少动态", { timeout: 45000 }, async () => {
	const root = await mkdtemp(join(await testRoot(), "settings-tabs-"));
	const clients = new Set<ServerResponse>();
	const port = await freePort();
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions: [], stage: "settings-tabs-test" }),
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
			if (path === "/api/settings") data = { theme: "light" };
			else if (path === "/api/auth") data = { providers: [] };
			else if (path === "/api/models")
				data = {
					models: [],
					catalog: [],
					defaultModel: {
						provider: "deepseek",
						id: "deepseek-flash",
						name: "DeepSeek V4.1 Flash",
						credentialReady: false,
						catalogPresent: true,
					},
					enabled: null,
					unavailableEnabled: [],
				};
			else if (path === "/api/sessions") data = { sessions: [] };
			res.writeHead(200, { "Content-Type": "application/json", Connection: "close" });
			res.end(JSON.stringify(data));
			return true;
		},
	);
	await listenWebServer(server, port);
	let browser: BrowserProbe | undefined;
	try {
		browser = await BrowserProbe.launch(root);
		await browser.call("Page.addScriptToEvaluateOnNewDocument", {
			source: `(() => {
			const start = document.startViewTransition.bind(document);
			document.startViewTransition = (...args) => {
				const transition = start(...args);
				transition.ready.then(() => {
					if (!document.querySelector('.settings-workspace[data-settings-slide]')) return;
					const animations = document.getAnimations().filter(a=>a.effect?.pseudoElement?.includes('settings-page') || a.effect?.pseudoElement?.includes('settings-tab-indicator'));
					const motions = animations.map(a=>{
						const frames=a.effect.getKeyframes(), from=new DOMMatrix(frames[0].transform || 'none'), to=new DOMMatrix(frames.at(-1).transform || 'none');
						if (!matchMedia('(prefers-reduced-motion: reduce)').matches) { a.pause(); a.currentTime=62.5; }
						return {pseudo:a.effect.pseudoElement,duration:a.effect.getTiming().duration,easing:frames[0].easing,fromX:from.m41,fromY:from.m42,toX:to.m41,toY:to.m42,frames};
					});
					window.__settingsSlide={path:location.pathname,direction:document.querySelector('.settings-workspace').dataset.settingsSlide,motions};
				}).catch(()=>{});
				return transition;
			};
		})()`,
		});
		await browser.navigate(`http://127.0.0.1:${port}/settings/general`);
		await browser.until("document.querySelector('.settings-route')?.innerText.includes('界面主题')", "常规设置就绪");
		const evidence: Slide[] = [];
		for (const [path, direction] of [
			["/settings", "forward"],
			["/settings/models", "forward"],
			["/settings/shortcuts", "forward"],
			["/settings/general", "backward"],
			["/settings/shortcuts", "forward"],
		]) {
			await browser.click(`.settings-nav-link[href="${path}"]`);
			await browser.until(`window.__settingsSlide?.path===${JSON.stringify(path)}`, "捕获双向快照");
			const slide: Slide = await browser.evaluate<Slide>("window.__settingsSlide");
			evidence.push(slide);
			assert.equal(slide.direction, direction);
			const outgoing = slide.motions.find((m) => m.pseudo === "::view-transition-old(settings-page)");
			const incoming = slide.motions.find((m) => m.pseudo === "::view-transition-new(settings-page)");
			assert.ok(outgoing && incoming, "新旧内容同时衔接");
			assert.equal(outgoing.duration, 250);
			assert.equal(incoming.duration, 250);
			assert.equal(outgoing.easing, "cubic-bezier(0.22, 1, 0.36, 1)");
			assert.equal(incoming.easing, outgoing.easing);
			const sign = direction === "forward" ? 1 : -1;
			assert.equal(outgoing.toX, -8 * sign);
			assert.equal(incoming.fromX, 8 * sign);
			assert.equal(outgoing.toY, 0);
			assert.equal(incoming.fromY, 0);
			assert.equal(incoming.frames[0].filter, undefined, "分类快照不使用模糊");
			assert.ok(
				slide.motions.some(
					(m) => m.pseudo === "::view-transition-group(settings-tab-indicator)" && m.duration === 250,
				),
				"下划线同步滑动",
			);
			if (path === "/settings") await browser.screenshot(join(root, "desktop-light-mid.png"));
			await browser.evaluate("document.getAnimations().forEach(a=>a.finish())");
			await browser.until("document.getAnimations().length===0", "分类切换结束");
			assert.equal(
				await browser.evaluate(
					"document.querySelector('.settings-nav-indicator').parentElement.getAttribute('href')",
				),
				path,
			);
			assert.equal(
				await browser.evaluate("document.querySelector('.settings-route').innerText.includes('正在加载')"),
				false,
			);
		}
		await browser.evaluate("history.back()");
		await browser.until(
			"location.pathname==='/settings/general' && document.querySelector('.settings-workspace').dataset.settingsSlide==='backward'",
			"历史后退沿菜单方向",
		);
		await browser.evaluate("document.getAnimations().forEach(a=>a.finish())");
		await browser.until("document.getAnimations().length===0", "历史后退结束");
		await browser.evaluate("history.forward()");
		await browser.until(
			"location.pathname==='/settings/shortcuts' && document.querySelector('.settings-workspace').dataset.settingsSlide==='forward'",
			"历史前进沿菜单方向",
		);
		await browser.evaluate("document.getAnimations().forEach(a=>a.finish())");
		await browser.until("document.getAnimations().length===0", "历史前进结束");

		await browser.evaluate(
			"document.documentElement.dataset.theme='dark';document.querySelector('.settings-nav-link[href=\"/settings/models\"]').focus()",
		);
		await browser.call("Input.dispatchKeyEvent", {
			type: "keyDown",
			key: "Enter",
			code: "Enter",
			text: "\r",
			windowsVirtualKeyCode: 13,
		});
		await browser.call("Input.dispatchKeyEvent", {
			type: "keyUp",
			key: "Enter",
			code: "Enter",
			windowsVirtualKeyCode: 13,
		});
		await browser.until("window.__settingsSlide?.path==='/settings/models'", "键盘切换模型偏好");
		await browser.screenshot(join(root, "desktop-dark-mid.png"));
		await browser.evaluate("document.getAnimations().forEach(a=>a.finish())");
		await browser.until("document.getAnimations().length===0", "键盘切换结束");
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 390,
			height: 844,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await browser.click('.sidebar button[aria-label="收起侧栏"]');
		await browser.until("document.getAnimations().length===0", "窄屏侧栏布局稳定");
		await browser.click('.settings-nav-link[href="/settings"]');
		await browser.until("window.__settingsSlide?.path==='/settings'", "窄屏左右切换");
		await browser.screenshot(join(root, "mobile-dark-mid.png"));
		await browser.evaluate("document.getAnimations().forEach(a=>a.finish())");
		await browser.until("document.getAnimations().length===0", "窄屏切换结束");
		assert.equal(
			await browser.evaluate(
				"document.documentElement.scrollWidth<=innerWidth && document.querySelector('.settings-nav').scrollWidth<=document.querySelector('.settings-nav').clientWidth",
			),
			true,
			"四菜单在窄屏完整显示",
		);

		await browser.evaluate(
			`for (const path of ["/settings/general","/settings/models","/settings/shortcuts"]) document.querySelector(\`.settings-nav-link[href="\${path}"]\`).click()`,
		);
		await browser.until(
			"location.pathname==='/settings/shortcuts' && window.__settingsSlide?.path==='/settings/shortcuts'",
			"快速连续切换保留最后目标",
		);
		await browser.evaluate("document.getAnimations().forEach(a=>a.finish())");
		await browser.until("document.getAnimations().length===0", "快速切换结束");
		assert.equal(
			await browser.evaluate("document.querySelector('.settings-nav-indicator').parentElement.getAttribute('href')"),
			"/settings/shortcuts",
		);
		await browser.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "reduce" }],
		});
		await browser.click('.settings-nav-link[href="/settings/general"]');
		await browser.until(
			"location.pathname==='/settings/general' && !document.getAnimations().some(a=>a.playState==='running')",
			"减少动态直接完成",
		);
		assert.equal(await browser.evaluate("getComputedStyle(document.querySelector('.settings-route')).opacity"), "1");
		await browser.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "no-preference" }],
		});
		await browser.call("Page.addScriptToEvaluateOnNewDocument", { source: "document.startViewTransition=undefined" });
		await browser.navigate(`http://127.0.0.1:${port}/settings/general`);
		await browser.click('.settings-nav-link[href="/settings/shortcuts"]');
		await browser.until(
			"location.pathname==='/settings/shortcuts' && document.querySelector('.settings-workspace')?.dataset.settingsSlide==='forward' && document.querySelector('.settings-route h2')?.textContent==='快捷键'",
			"无快照时导航可用",
		);
		assert.equal(
			await browser.evaluate("getComputedStyle(document.querySelector('.settings-route')).animationName"),
			"settings-page-slide-in",
			"无快照时保留横向入场",
		);
		await writeFile(join(root, "evidence.json"), JSON.stringify(evidence, null, 2));
		console.log("SETTINGS_TABS_EVIDENCE", root);
	} finally {
		await browser?.close();
		for (const client of clients) client.destroy();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	}
});
