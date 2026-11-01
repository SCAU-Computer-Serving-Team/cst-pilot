import assert from "node:assert/strict";
import { mkdir, mkdtemp } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { configureTestModel, testRoot } from "./support.ts";

test("导出预览：后台加载保留旧文档，就绪后淡入，连续切换与失败不闪空", { timeout: 90_000 }, async () => {
	const root = await mkdtemp(join(await testRoot(), "export-preview-"));
	const home = join(root, "home");
	await mkdir(home);
	await configureTestModel(home);
	const pool = new WebSessionPool({
		cwd: root,
		agentDir: home,
		sessionDir: join(home, "sessions"),
		withLoader: (load) => load(),
	});
	const slot = await pool.create();
	slot.session.sessionManager.appendMessage({
		role: "user",
		content: [
			{ type: "text", text: "预览连续性检查\n".repeat(100) },
			{
				type: "image",
				mimeType: "image/png",
				data: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a3foAAAAASUVORK5CYII=",
			},
		],
		timestamp: Date.now(),
	});
	const port = await freePort();
	const api = createWebApi(pool, home, port);
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions: [], stage: "export-preview" }),
		(...args) => api(...args),
	);
	await listenWebServer(server, port);
	let browser: BrowserProbe | undefined;
	try {
		browser = await BrowserProbe.launch(root);
		const probe = browser;
		await browser.call("Page.addScriptToEvaluateOnNewDocument", {
			source: `
			const decode = HTMLImageElement.prototype.decode;
			HTMLImageElement.prototype.decode = async function() {
				if (parent !== window && parent.holdPreviewAssets) {
					parent.previewAssetWaits++;
					await new Promise(resolve => parent.releasePreviewAssets = resolve);
				}
				if (parent !== window && parent.failPreviewAssets) throw new Error('fixture image failure');
				return decode.call(this);
			};
		`,
		});
		await browser.navigate(`http://127.0.0.1:${port}/s/${slot.id}/export`);
		await browser.until(
			"document.querySelector('iframe')?.contentDocument?.documentElement?.dataset.exportState==='ready' && getComputedStyle(document.querySelector('.export-preview-viewport')).visibility==='visible'",
			"首次预览就绪",
		);
		await browser.evaluate(`(() => {
			window.previewRequests = [];
			const original = window.fetch;
			window.originalFetch = original;
			window.fetch = async (url, init) => {
				if (!String(url).endsWith('/exports/preview')) return original(url, init);
				await new Promise(resolve => window.previewRequests.push(resolve));
				return original(url, init);
			};
			window.holdPreviewAssets = true;
			window.previewAssetWaits = 0;
			window.pausePreviewMotion = true;
			window.addEventListener('animationstart', event => {
				if (event.animationName !== 'export-preview-reveal' || !window.pausePreviewMotion) return;
				window.previewAnimation = event.target.getAnimations()[0];
				window.previewAnimation.pause();
			});
			window.originalPreview = document.querySelector('iframe');
			window.originalPreview.contentWindow.scrollTo(0, 80);
		})()`);
		async function select(theme: string) {
			await probe.click(".export-select:nth-of-type(2) .export-select-trigger");
			await probe.click(`.export-select-popover [role=option][data-key=${theme}]`);
		}
		await select("dark");
		await browser.until("window.previewRequests.length===1", "延迟预览请求");
		assert.equal(
			await browser.evaluate("getComputedStyle(document.querySelector('.export-preview-viewport')).visibility"),
			"visible",
			"新预览请求期间旧文档始终可见",
		);
		assert.equal(
			await browser.evaluate(
				"window.originalPreview.isConnected && window.originalPreview.contentDocument.documentElement.dataset.theme==='light'",
			),
			true,
		);
		await browser.evaluate("window.previewRequests.shift()()");
		await browser.until("window.previewAssetWaits===1", "新文档等待图片就绪");
		assert.equal(await browser.evaluate("document.querySelectorAll('iframe').length"), 2);
		assert.equal(
			await browser.evaluate("getComputedStyle(document.querySelector('iframe:last-child')).opacity"),
			"0",
			"资源未就绪时新文档保持隐藏",
		);
		assert.equal(
			await browser.evaluate(
				"window.originalPreview.isConnected && getComputedStyle(window.originalPreview).visibility==='visible'",
			),
			true,
		);
		await browser.evaluate("window.holdPreviewAssets=false; window.releasePreviewAssets()");
		await browser.until("!!window.previewAnimation", "资源就绪立即开始淡入");
		const motion = await browser.evaluate<{
			duration: number;
			frames: { opacity: number; oldOpacity: number; filter: string }[];
			scroll: number;
		}>(`(() => {
			const animation = window.previewAnimation, incoming = document.querySelector('iframe:last-child');
			const duration = animation.effect.getTiming().duration;
			const frames = [0, duration/2, duration].map(time => {
				animation.currentTime=time;
				return {opacity:Number(getComputedStyle(incoming).opacity), oldOpacity:Number(getComputedStyle(window.originalPreview).opacity), filter:getComputedStyle(incoming).filter};
			});
			animation.currentTime=duration/2;
			return {duration, frames, scroll:incoming.contentWindow.scrollY};
		})()`);
		assert.equal(motion.duration, 250);
		assert.equal(motion.frames[0].opacity, 0);
		assert.ok(motion.frames[1].opacity > 0 && motion.frames[1].opacity < 1);
		assert.equal(motion.frames[2].opacity, 1);
		assert.ok(motion.frames.every((frame) => frame.oldOpacity === 1 && frame.filter === "none"));
		assert.equal(motion.scroll, 80, "交接保留原滚动位置");
		await browser.screenshot(join(root, "theme-midpoint.png"));
		await browser.evaluate("window.previewAnimation.finish(); window.pausePreviewMotion=false");
		await browser.until(
			"document.querySelectorAll('iframe').length===1 && document.querySelector('iframe').contentDocument.documentElement.dataset.theme==='dark'",
			"夜间预览完成交接",
		);
		await browser.screenshot(join(root, "dark.png"));
		// 连续切换时只接收最后一次请求，加载中始终保留完成态。
		await select("light");
		await select("dark");
		await browser.until("window.previewRequests.length===2", "连续主题请求");
		await browser.evaluate("window.previewRequests.pop()()");
		await browser.until("!document.querySelector('.export-generate').disabled", "最新请求完成");
		await browser.evaluate("window.previewRequests.shift()()");
		await browser.until(
			"document.querySelectorAll('iframe').length===1 && document.querySelector('iframe').contentDocument.documentElement.dataset.theme==='dark'",
			"过期响应不覆盖主题",
		);
		// 新选择会移除仍在等待资源的旧候选，迟到的就绪消息不能触发切换。
		await browser.evaluate("window.holdPreviewAssets=true");
		await select("light");
		await browser.until("window.previewRequests.length===1", "待加载候选请求");
		await browser.evaluate("window.previewRequests.shift()()");
		await browser.until("window.previewAssetWaits===2", "候选文档仍在等待资源");
		await select("dark");
		await browser.until(
			"window.previewRequests.length===1 && document.querySelectorAll('iframe').length===1",
			"新选择撤下未完成候选",
		);
		await browser.evaluate(
			"window.holdPreviewAssets=false; window.releasePreviewAssets(); window.previewRequests.shift()()",
		);
		await browser.until(
			"!document.querySelector('.export-generate').disabled && document.querySelectorAll('iframe').length===1 && document.querySelector('iframe').contentDocument.documentElement.dataset.theme==='dark'",
			"最终主题保持夜间",
		);
		// 模拟 API 失败，旧预览与重试入口保留。
		await browser.evaluate(
			`window.fetch = async (url, init) => String(url).endsWith('/exports/preview') ? new Response(JSON.stringify({error:{code:'fixture',message:'预览请求失败'}}), {status:503,headers:{'Content-Type':'application/json'}}) : window.originalFetch(url, init)`,
		);
		await select("light");
		await browser.until("!!document.querySelector('.export-preview-panel [role=alert]')", "预览失败提示");
		assert.equal(
			await browser.evaluate("getComputedStyle(document.querySelector('.export-preview-viewport')).visibility"),
			"visible",
		);
		assert.equal(
			await browser.evaluate("document.querySelector('iframe').contentDocument.documentElement.dataset.theme"),
			"dark",
		);
		await browser.evaluate("window.fetch=window.originalFetch; window.failPreviewAssets=true");
		await browser.click(".export-preview-panel .notice-action");
		await browser.until(
			"document.querySelector('.error-details')?.textContent.includes('导出内容或字体') && document.querySelectorAll('iframe').length===1",
			"资源加载失败保留旧文档",
		);
		assert.equal(
			await browser.evaluate("document.querySelector('iframe').contentDocument.documentElement.dataset.theme"),
			"dark",
		);
		await browser.evaluate("window.failPreviewAssets=false");
		await browser.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "reduce" }],
		});
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 390,
			height: 900,
			deviceScaleFactor: 1,
			mobile: false,
		});
		await browser.click('[aria-label="收起侧栏"]');
		await browser.until("!document.querySelector('.sidebar-backdrop')", "窄屏收起侧栏");
		await browser.click(".export-preview-panel .notice-action");
		await browser.until(
			"document.querySelectorAll('iframe').length===1 && document.querySelector('iframe').contentDocument.documentElement.dataset.theme==='light'",
			"减少动态时就绪后直接交接",
		);
		assert.equal(await browser.evaluate("document.querySelector('iframe').getAnimations().length"), 0);
		assert.equal(await browser.evaluate("document.documentElement.scrollWidth<=innerWidth"), true);
		await browser.screenshot(join(root, "390-reduced-motion.png"));
	} finally {
		await browser?.close();
		await api.close();
		await pool.close();
		await pool.catalog.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	}
});
