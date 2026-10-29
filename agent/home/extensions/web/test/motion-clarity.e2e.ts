import assert from "node:assert/strict";
import { mkdtemp, writeFile } from "node:fs/promises";
import type { ServerResponse } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

test("转场与状态动效保持清晰，登录页静态背景保留模糊", { timeout: 45000 }, async () => {
	const root = await mkdtemp(join(await testRoot(), "motion-clarity-"));
	const clients = new Set<ServerResponse>();
	const port = await freePort();
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions: [], stage: "motion-clarity-test" }),
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
			else if (path === "/api/models") data = { models: [], enabled: null, unavailableEnabled: [] };
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
		await browser.navigate(`http://127.0.0.1:${port}/settings/general`);
		await browser.until("document.querySelector('.settings-route')?.innerText.includes('界面主题')", "设置就绪");
		const keyframes = await browser.evaluate<{ names: string[]; blurred: string[] }>(`(() => {
			const names=[], blurred=[];
			const probe=document.createElement('div'); probe.hidden=true; document.body.append(probe);
			function inspect(rules) {
				for (const rule of rules) {
					if (rule instanceof CSSKeyframesRule) {
						names.push(rule.name);
						for (const frame of rule.cssRules) {
							probe.style.filter=frame.style.filter || 'none';
							probe.style.backdropFilter=frame.style.backdropFilter || 'none';
							const style=getComputedStyle(probe), value=style.filter+' '+style.backdropFilter;
							if ([...value.matchAll(/blur\\(([^)]+)\\)/g)].some(match=>Number.parseFloat(match[1])>0)) blurred.push(rule.name+': '+value);
						}
					} else if (rule.cssRules) inspect(rule.cssRules);
				}
			}
			for (const sheet of document.styleSheets) inspect(sheet.cssRules);
			probe.remove();
			return {names,blurred};
		})()`);
		for (const name of [
			"chat-content-reveal",
			"part-in",
			"route-part-in",
			"t-stream-in",
			"notification-in",
			"notification-out",
			"home-mark-in",
			"home-text-out",
			"composer-in",
			"text-surface-in",
			"special-status-in",
			"settings-page-slide-in",
			"settings-page-slide-out",
		]) {
			assert.ok(keyframes.names.includes(name), `实际加载 ${name} 动效`);
		}
		assert.deepEqual(keyframes.blurred, [], "全部已加载关键帧均无高斯模糊");
		await browser.evaluate(`(() => {
			const start=document.startViewTransition.bind(document);
			document.startViewTransition=(...args)=>{
				const transition=start(...args);
				transition.ready.then(()=>{
					window.__clarityFrames=document.getAnimations().flatMap(a=>a.effect.getKeyframes());
				}).catch(()=>{});
				return transition;
			};
		})()`);
		await browser.click('.settings-nav-link[href="/settings/shortcuts"]');
		await browser.until("Array.isArray(window.__clarityFrames)", "实际转场快照");
		const frames = await browser.evaluate<{ filter?: string; backdropFilter?: string }[]>("window.__clarityFrames");
		assert.ok(frames.length > 0);
		assert.ok(frames.every((frame) => !/blur\s*\(/.test(`${frame.filter ?? ""} ${frame.backdropFilter ?? ""}`)));
		await browser.until("document.getAnimations().length===0", "转场结束");
		await browser.screenshot(join(root, "desktop-clear.png"));
		await browser.navigate(`http://127.0.0.1:${port}/login`);
		assert.equal(
			await browser.evaluate("getComputedStyle(document.querySelector('.login-behind')).filter"),
			"blur(20px)",
			"静态主页背景保持 20px 模糊",
		);
		assert.equal(
			await browser.evaluate("getComputedStyle(document.querySelector('.login-panel')).filter"),
			"none",
			"登录面板保持清晰",
		);
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 390,
			height: 844,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await browser.screenshot(join(root, "mobile-login.png"));
		await writeFile(join(root, "evidence.json"), JSON.stringify({ keyframes, frames }, null, 2));
		console.log("MOTION_CLARITY_EVIDENCE", root);
	} finally {
		await browser?.close();
		for (const client of clients) client.destroy();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	}
});
