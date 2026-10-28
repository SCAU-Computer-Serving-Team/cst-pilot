import assert from "node:assert/strict";
import { mkdtemp, writeFile } from "node:fs/promises";
import type { ServerResponse } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

test("会话导航即时反馈、取消按下、修饰键、键盘、快速切换与读取失败恢复", { timeout: 45000 }, async () => {
	const root = await mkdtemp(join(await testRoot(), "session-navigation-"));
	const now = Date.now();
	const sessions = ["one", "two", "failed"].map((id) => ({
		id,
		title: id === "failed" ? "读取失败" : "你好",
		updatedAt: new Date(now).toISOString(),
		running: false,
	}));
	const clients = new Set<ServerResponse>();
	const port = await freePort();
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions, stage: "navigation-test" }),
		async (_req, res, path) => {
			if (!path.startsWith("/api/")) return false;
			if (path.endsWith("/events")) {
				res.writeHead(200, { "Content-Type": "text/event-stream" });
				res.write(": connected\n\n");
				clients.add(res);
				res.on("close", () => clients.delete(res));
				return true;
			}
			const match = /^\/api\/sessions\/([^/]+)$/.exec(path);
			let data: unknown = {};
			let status = 200;
			if (match) {
				const id = match[1];
				if (id === "failed") {
					status = 503;
					data = { error: "会话读取失败" };
				} else {
					const message = { role: "user", content: `问题 ${id}`, timestamp: now };
					const answer = {
						role: "assistant",
						content: [{ type: "text", text: "你好，我可以协助检查电脑状态。".repeat(80) }],
						timestamp: now + 1,
						stopReason: "stop",
					};
					data = {
						id,
						messages: [message, answer],
						entries: [
							{ id: "u", message },
							{ id: "a", message: answer },
						],
						running: false,
						queue: { version: 0, items: [], paused: false },
						ui: [],
					};
				}
			} else if (path === "/api/sessions") data = { sessions };
			else if (path === "/api/models") data = { models: [], selected: null, enabled: null, thinkingLevels: [] };
			else if (path === "/api/settings") data = { theme: "light", providers: [], models: [], scopedModels: [] };
			else if (path === "/api/auth") data = { providers: [] };
			res.writeHead(status, { "Content-Type": "application/json", Connection: "close" });
			res.end(JSON.stringify(data));
			return true;
		},
	);
	await listenWebServer(server, port);
	let browser: BrowserProbe | undefined;
	const row = (id: string) => `.sidebar-session[data-session-id="${id}"]`;
	try {
		browser = await BrowserProbe.launch(root);
		await browser.navigate(`http://127.0.0.1:${port}/`);
		await browser.until("!!document.querySelector('.sidebar-session')", "会话列表");
		const point = await browser.evaluate<{ x: number; y: number }>(`(() => {
			const rect = document.querySelector('${row("two")}').getBoundingClientRect();
			return {x: rect.left + 50, y: rect.top + rect.height / 2};
		})()`);
		const originalSelection = await browser.evaluate("document.querySelector('.sidebar-selection').style.transform");
		await browser.call("Input.dispatchMouseEvent", { type: "mouseMoved", ...point });
		await browser.call("Input.dispatchMouseEvent", {
			type: "mousePressed",
			button: "left",
			clickCount: 1,
			...point,
		});
		const pressed = await browser.evaluate<{ background: string; opacity: number }>(`(() => {
			const style = getComputedStyle(document.querySelector('${row("two")}'));
			return {background:style.backgroundColor,opacity:Number(style.opacity)};
		})()`);
		await browser.screenshot(join(root, "pressed.png"));
		await browser.call("Input.dispatchMouseEvent", { type: "mouseMoved", x: 900, y: 500, buttons: 1 });
		await browser.call("Input.dispatchMouseEvent", {
			type: "mouseReleased",
			button: "left",
			clickCount: 1,
			x: 900,
			y: 500,
		});
		assert.equal(await browser.evaluate("location.pathname"), "/", "移出后松开不导航");
		assert.equal(
			await browser.evaluate("document.querySelector('.sidebar-selection').style.transform"),
			originalSelection,
			"取消按下不改变选区",
		);
		for (const cancelled of [false, true]) {
			await browser.evaluate(`(() => {
				const row = document.querySelector('${row("two")}');
				const listenerTarget = ${cancelled} ? row : document;
				listenerTarget.addEventListener('click', e => e.preventDefault(), {once:true});
				row.dispatchEvent(new MouseEvent('click', {bubbles:true,cancelable:true,ctrlKey:${!cancelled}}));
			})()`);
			assert.equal(await browser.evaluate("location.pathname"), "/");
			assert.equal(
				await browser.evaluate("document.querySelector('.sidebar-selection').style.transform"),
				originalSelection,
			);
		}
		await browser.evaluate(`(() => {
			window.__clickFeedback = [];
			document.addEventListener('click', e => {
				const row = e.target.closest('.sidebar-session');
				if (!row || e.ctrlKey) return;
				const top = document.querySelector('.sidebar-top');
				const selection = document.querySelector('.sidebar-selection');
				const expected = row.getBoundingClientRect().top - top.getBoundingClientRect().top + top.scrollTop;
				window.__clickFeedback.push({id:row.dataset.sessionId, immediate:Math.abs(new DOMMatrix(selection.style.transform).m42 - expected)<1});
			});
		})()`);
		await browser.click(row("one"));
		await browser.until("document.querySelector('.chat-loaded')?.dataset.sessionId === 'one'", "鼠标打开会话");
		const historyElements = await browser.evaluate<number>(
			"document.querySelectorAll('.chat-loaded .markdown *').length",
		);
		assert.ok(historyElements < 30, `历史正文使用块级节点，避免逐字创建 ${historyElements} 个元素`);
		await browser.evaluate(`document.querySelector('${row("two")}').focus()`);
		await browser.call("Input.dispatchKeyEvent", {
			type: "keyDown",
			key: "Enter",
			code: "Enter",
			windowsVirtualKeyCode: 13,
		});
		await browser.call("Input.dispatchKeyEvent", {
			type: "keyUp",
			key: "Enter",
			code: "Enter",
			windowsVirtualKeyCode: 13,
		});
		await browser.until("document.querySelector('.chat-loaded')?.dataset.sessionId === 'two'", "键盘打开会话");
		assert.equal(
			await browser.evaluate(`document.querySelector('${row("two")}').getAttribute('aria-current')`),
			"page",
		);
		await browser.evaluate(
			`document.querySelector('${row("one")}').click();document.querySelector('${row("two")}').click();document.querySelector('${row("one")}').click()`,
		);
		await browser.until(
			"document.querySelector('.chat-loaded')?.dataset.sessionId === 'one'",
			"连续切换以最后一次为准",
		);
		await browser.evaluate("history.back()");
		await browser.until("document.querySelector('.chat-loaded')?.dataset.sessionId === 'two'", "返回上一会话");
		await browser.click(row("failed"));
		await browser.until(
			"location.pathname === '/s/failed' && !!document.querySelector('.error-notice')",
			"读取失败保留恢复入口",
		);
		await browser.click(row("one"));
		await browser.until(
			"document.querySelector('.chat-loaded')?.dataset.sessionId === 'one'",
			"失败后仍可打开其他会话",
		);
		await browser.until(
			`Math.abs(document.querySelector('.sidebar-selection').getBoundingClientRect().top-document.querySelector('${row("one")}').getBoundingClientRect().top)<1`,
			"选区最终跟随当前会话",
		);
		await browser.evaluate(
			"Promise.all(document.getAnimations().filter(a=>a.effect?.getTiming().iterations!==Infinity).map(a=>a.finished.catch(()=>{})))",
		);
		const motion = [];
		for (const id of ["two", "one"]) {
			motion.push(
				await browser.evaluate<{ id: string; intermediate: number; contentAnimated: boolean }>(`(async () => {
				const pill = document.querySelector('.sidebar-selection');
				const row = document.querySelector('${row(id)}');
				const from = pill.getBoundingClientRect().top, to = row.getBoundingClientRect().top;
				const positions = new Set();
				let contentAnimated = false;
				const start = performance.now();
				row.click();
				while (performance.now() - start < 350) {
					await new Promise(r => requestAnimationFrame(r));
					const y = pill.getBoundingClientRect().top;
					if (y > Math.min(from,to) + .1 && y < Math.max(from,to) - .1) positions.add(Math.round(y*10));
					const chat = document.querySelector('.chat-loaded');
					if (chat?.dataset.sessionId === '${id}' && Number(getComputedStyle(chat).opacity) < .99) contentAnimated = true;
				}
				return {id:'${id}', intermediate:positions.size, contentAnimated};
			})()`),
			);
		}
		const resized = await browser.evaluate<{ before: number; after: number }>(`(async () => {
			const top = document.querySelector('.sidebar-top'), pill = document.querySelector('.sidebar-selection');
			document.querySelector('${row("two")}').click();
			for (let i=0;i<10 && document.querySelector('.chat-loaded')?.dataset.sessionId!=='two';i++) await new Promise(r=>requestAnimationFrame(r));
			await new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)));
			const before = pill.getAnimations().length;
			top.style.width = (top.clientWidth-1)+'px';
			await new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)));
			const after = pill.getAnimations().length;
			top.style.width = '';
			return {before,after};
		})()`);
		const feedback = await browser.evaluate<{ id: string; immediate: boolean }[]>("window.__clickFeedback");
		assert.ok(feedback.length >= 6);
		assert.ok(
			feedback.every((value) => value.immediate),
			"鼠标与键盘均在点击处理时启动选区移动",
		);
		await browser.screenshot(join(root, "selected.png"));
		await writeFile(join(root, "evidence.json"), JSON.stringify({ feedback, pressed, motion, resized }, null, 2));
		console.log("NAVIGATION_EVIDENCE", root);
		assert.equal(pressed.background, "rgba(0, 0, 0, 0)", "按下不叠加第二块背景，避免松开时闪烁");
		assert.ok(pressed.opacity < 1, "按下反馈只作用于行内内容");
		assert.ok(
			motion.every((sample) => sample.intermediate >= 3),
			"两个同名会话来回切换均有连续滑动帧",
		);
		assert.ok(
			motion.every((sample) => sample.contentAnimated),
			"两个同名会话来回切换均实际绘制正文淡入",
		);
		assert.ok(resized.before > 0 && resized.after > 0, "宽度变化且目标位置未变时，滑动继续播放");
	} finally {
		await browser?.close();
		for (const client of clients) client.end();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	}
});
