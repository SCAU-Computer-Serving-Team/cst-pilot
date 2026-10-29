import assert from "node:assert/strict";
import { mkdtemp, writeFile } from "node:fs/promises";
import type { ServerResponse } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

test("消息、工具与草稿图片共用原位缩放预览，支持键盘、深色、窄屏与减少动态", { timeout: 45000 }, async () => {
	const root = await mkdtemp(join(await testRoot(), "image-preview-"));
	const now = Date.now();
	const sessions = [{ id: "pictures", title: "图片预览", updatedAt: new Date(now).toISOString(), running: false }];
	let png = "";
	const clients = new Set<ServerResponse>();
	const port = await freePort();
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions, stage: "image-preview-test" }),
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
			if (path === "/api/sessions/pictures") {
				const messages = [
					{
						role: "user",
						content: [
							{ type: "text", text: "查看这张图片" },
							{ type: "image", mimeType: "image/png", data: png },
						],
						timestamp: now,
					},
					{
						role: "assistant",
						content: [{ type: "toolCall", id: "picture", name: "read", arguments: { path: "preview.png" } }],
						timestamp: now + 1,
						stopReason: "toolUse",
					},
					{
						role: "toolResult",
						toolCallId: "picture",
						toolName: "read",
						content: [{ type: "image", mimeType: "image/png", data: png }],
						timestamp: now + 2,
					},
					{
						role: "assistant",
						content: [{ type: "text", text: "图片已读取。" }],
						timestamp: now + 3,
						stopReason: "stop",
					},
				];
				data = {
					id: "pictures",
					messages,
					entries: messages.map((message, i) => ({ id: `entry-${i}`, message })),
					running: false,
					queue: { version: 0, items: [], paused: false },
					ui: [],
				};
			} else if (path === "/api/sessions") data = { sessions };
			else if (path === "/api/models") data = { models: [], selected: null, enabled: null, thinkingLevels: [] };
			else if (path === "/api/settings") data = { theme: "light", providers: [], models: [], scopedModels: [] };
			else if (path === "/api/auth") data = { providers: [] };
			res.writeHead(200, { "Content-Type": "application/json", Connection: "close" });
			res.end(JSON.stringify(data));
			return true;
		},
	);
	await listenWebServer(server, port);
	let browser: BrowserProbe | undefined;
	try {
		browser = await BrowserProbe.launch(root);
		png = await browser.evaluate<string>(`(() => {
			const canvas = document.createElement('canvas'); canvas.width = 1200; canvas.height = 720;
			const ctx = canvas.getContext('2d'); ctx.fillStyle = '#f4f7f9'; ctx.fillRect(0,0,1200,720);
			ctx.fillStyle = '#1a2024'; ctx.font = '48px sans-serif'; ctx.fillText('Image preview / 1200 × 720',64,100);
			for(let y=0;y<5;y++)for(let x=0;x<12;x++){ctx.fillStyle=['#e0e7ec','#b4a9f9','#8e7bf4','#6246d8'][(x+y)%4];ctx.fillRect(64+x*88,180+y*88,72,72);}
			return canvas.toDataURL('image/png').split(',')[1];
		})()`);
		await browser.navigate(`http://127.0.0.1:${port}/s/pictures`);
		await browser.until("document.querySelector('.message-image')?.naturalWidth === 1200", "历史消息图片");
		const original = await browser.evaluate("document.querySelector('.conversation').scrollTop");
		await browser.evaluate("document.querySelector('.message-image').parentElement.focus()");
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
		await browser.until("!!document.querySelector('.image-preview[open]')", "消息图片预览");
		const opening = await browser.evaluate<{
			modal: boolean;
			animation: { transform: string }[][];
			background: string;
			timings: number[];
		}>(`(() => {
			const modal = document.querySelector('.image-preview');
			return {modal:modal.matches(':modal'), animation:document.querySelector('.image-preview-zoom').getAnimations().map(a=>a.effect.getKeyframes()), background:getComputedStyle(document.querySelector('.image-preview-scrim')).backgroundColor,timings:[...document.querySelectorAll('.image-preview-zoom, .image-preview-scrim')].flatMap(el=>el.getAnimations().map(a=>a.effect.getTiming().duration))};
		})()`);
		assert.equal(opening.modal, true, "原生模态限制焦点并隔离背景操作");
		assert.deepEqual(opening.timings, [350, 350], "图片与背景同步放慢至 350ms");
		assert.ok(opening.animation.length > 0 && opening.animation[0][0].transform !== "none", "由原图位置缩放入场");
		assert.equal(opening.background, "rgb(255, 255, 255)", "浅色使用纯白预览背景");
		assert.equal(
			await browser.evaluate("document.querySelector('.message-image').style.visibility"),
			"hidden",
			"预览期间隐藏原图，保留布局",
		);
		await browser.evaluate(
			"Promise.all(document.querySelector('.image-preview-zoom').getAnimations().map(a=>a.finished))",
		);
		const desktop = await browser.evaluate<{ width: number; height: number; centered: boolean }>(`(() => {
			const rect = document.querySelector('.image-preview-zoom img').getBoundingClientRect();
			return {width:rect.width,height:rect.height,centered:Math.abs(rect.left+rect.width/2-innerWidth/2)<1 && Math.abs(rect.top+rect.height/2-innerHeight/2)<1};
		})()`);
		assert.ok(desktop.centered && desktop.width > 320 && desktop.width <= 1200);
		assert.ok(Math.abs(desktop.width / desktop.height - 1200 / 720) < 0.01);
		await browser.screenshot(join(root, "desktop-light.png"));
		await browser.call("Input.dispatchKeyEvent", {
			type: "keyDown",
			key: "Escape",
			code: "Escape",
			windowsVirtualKeyCode: 27,
		});
		await browser.call("Input.dispatchKeyEvent", {
			type: "keyUp",
			key: "Escape",
			code: "Escape",
			windowsVirtualKeyCode: 27,
		});
		await browser.until("!document.querySelector('.image-preview')", "Esc 收回图片");
		assert.equal(await browser.evaluate("document.querySelector('.message-image').style.visibility"), "");
		assert.equal(
			await browser.evaluate("document.activeElement === document.querySelector('.message-image').parentElement"),
			true,
			"关闭后焦点回到原图入口",
		);
		assert.equal(
			await browser.evaluate("document.querySelector('.conversation').scrollTop"),
			original,
			"预览不改变历史滚动位置",
		);
		assert.equal(await browser.evaluate("document.documentElement.style.overflow"), "", "关闭后恢复滚动");
		await browser.click(".turn-process-head");
		await browser.until("!!document.querySelector('.tool-image')", "工具返回图片");
		await browser.click(".tool-image");
		await browser.until("!!document.querySelector('.image-preview[open]')", "工具图片预览");
		await browser.click(".image-preview-zoom");
		await browser.until("!document.querySelector('.image-preview')", "点击图片收回");
		await browser.evaluate(`(async () => {
			const bytes = Uint8Array.from(atob('${png}'), c=>c.charCodeAt(0));
			const transfer = new DataTransfer(); transfer.items.add(new File([bytes], 'preview.png', {type:'image/png'}));
			document.querySelector('.composer-input').dispatchEvent(new ClipboardEvent('paste', {bubbles:true,cancelable:true,clipboardData:transfer}));
		})()`);
		await browser.until("!!document.querySelector('.draft-image-open img')?.naturalWidth", "草稿图片缩略图");
		await browser.evaluate("document.documentElement.dataset.theme='dark'");
		await browser.click(".draft-image-open");
		await browser.until("!!document.querySelector('.image-preview[open]')", "草稿图片共用预览");
		await browser.evaluate(
			"Promise.all(document.querySelector('.image-preview-zoom').getAnimations().map(a=>a.finished))",
		);
		assert.notEqual(
			await browser.evaluate("getComputedStyle(document.querySelector('.image-preview-scrim')).backgroundColor"),
			"rgb(255, 255, 255)",
			"深色背景随主题变化",
		);
		await browser.screenshot(join(root, "desktop-dark.png"));
		await browser.click(".image-preview-close");
		await browser.until("!document.querySelector('.image-preview')", "关闭按钮");
		const interrupted = await browser.evaluate<{ before: string; after: string; timings: number[] }>(`(async () => {
			document.querySelector('.draft-image-open').click();
			await new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)));
			const zoom = document.querySelector('.image-preview-zoom');
			const before = getComputedStyle(zoom).transform;
			document.querySelector('.image-preview-close').click();
			return {before,after:zoom.getAnimations()[0].effect.getKeyframes()[0].transform,timings:[...document.querySelectorAll('.image-preview-zoom, .image-preview-scrim')].flatMap(el=>el.getAnimations().map(a=>a.effect.getTiming().duration))};
		})()`);
		assert.equal(interrupted.after, interrupted.before, "缩放中关闭从当前呈现值收回");
		assert.deepEqual(interrupted.timings, [250, 250], "图片与背景同步以 250ms 收回");
		await browser.until("!document.querySelector('.image-preview')", "快速关闭完成");
		await browser.click(".draft-image-open");
		await browser.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "reduce" }],
		});
		await browser.until(
			"document.querySelector('.image-preview-zoom').getAnimations().length === 0",
			"偏好变化立即结束空间动效",
		);
		await browser.click(".image-preview-close");
		await browser.until("!document.querySelector('.image-preview')", "减少动态下关闭");
		await browser.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "no-preference" }],
		});
		await browser.click(".draft-image-open");
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 390,
			height: 844,
			deviceScaleFactor: 1,
			mobile: true,
		});
		await browser.until("!document.querySelector('.image-preview')", "窗口变化收回预览");
		await browser.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "reduce" }],
		});
		await browser.click('.sidebar button[aria-label="收起侧栏"]');
		await browser.click(".draft-image-open");
		await browser.until("!!document.querySelector('.image-preview[open]')", "窄屏减少动态预览");
		const mobile = await browser.evaluate<{ fits: boolean; animations: number }>(`(() => {
			const rect = document.querySelector('.image-preview-zoom img').getBoundingClientRect();
			return {fits:rect.left>=0 && rect.right<=innerWidth && rect.top>=0 && rect.bottom<=innerHeight,animations:document.querySelector('.image-preview-zoom').getAnimations().length};
		})()`);
		assert.ok(mobile.fits, "窄屏图片完整显示且不溢出");
		assert.equal(mobile.animations, 0, "减少动态时立即呈现");
		await browser.screenshot(join(root, "mobile-dark.png"));
		await browser.evaluate("document.querySelector('.image-preview').click()");
		await browser.until("!document.querySelector('.image-preview')", "点击空白关闭");
		await browser.evaluate(`(async () => {
			const canvas = document.createElement('canvas'); canvas.width=480; canvas.height=2400;
			const ctx = canvas.getContext('2d'); ctx.fillStyle='#f4f7f9'; ctx.fillRect(0,0,480,2400);
			const blob = await new Promise(r=>canvas.toBlob(r));
			const transfer = new DataTransfer(); transfer.items.add(new File([blob], 'portrait.png', {type:'image/png'}));
			document.querySelector('.composer-input').dispatchEvent(new ClipboardEvent('paste', {bubbles:true,cancelable:true,clipboardData:transfer}));
		})()`);
		await browser.until("document.querySelectorAll('.draft-image-open img')[1]?.naturalHeight === 2400", "长图草稿");
		await browser.click(".draft-image:nth-child(2) .draft-image-open");
		const portrait = await browser.evaluate<{ width: number; height: number }>(`(() => {
			const rect = document.querySelector('.image-preview-zoom img').getBoundingClientRect();
			return {width:rect.width,height:rect.height};
		})()`);
		assert.ok(
			portrait.height <= 796 && Math.abs(portrait.width / portrait.height - 0.2) < 0.01,
			"长图按高度适配并保留比例",
		);
		await browser.evaluate("document.querySelectorAll('.draft-image-remove')[1].click()");
		await browser.until("!document.querySelector('.image-preview')", "原图卸载时清理预览");
		assert.equal(await browser.evaluate("document.documentElement.style.overflow"), "", "原图卸载时恢复滚动");
		await writeFile(
			join(root, "evidence.json"),
			JSON.stringify({ opening, desktop, interrupted, mobile, portrait }, null, 2),
		);
		console.log("IMAGE_PREVIEW_EVIDENCE", root);
	} finally {
		await browser?.close();
		for (const client of clients) client.end();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	}
});
