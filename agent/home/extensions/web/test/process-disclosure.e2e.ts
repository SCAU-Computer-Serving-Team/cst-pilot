import assert from "node:assert/strict";
import { mkdtemp, writeFile } from "node:fs/promises";
import type { ServerResponse } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

test("历史过程首次展开才挂载，收起保留工具节点、分页与展开动画", { timeout: 30000 }, async () => {
	const root = await mkdtemp(join(await testRoot(), "process-disclosure-"));
	const now = Date.now();
	const sessions = [{ id: "history", title: "过程按需加载", updatedAt: new Date(now).toISOString(), running: false }];
	const messages = [
		{ role: "user", content: "检查进程", timestamp: now },
		{
			role: "assistant",
			content: [
				{ type: "thinking", thinking: "检查进程信息。" },
				{ type: "text", text: "检查过程正文。" },
				{ type: "toolCall", id: "processes", name: "sys", arguments: { scope: "process" } },
			],
			timestamp: now + 1,
			stopReason: "toolUse",
		},
		{
			role: "toolResult",
			toolCallId: "processes",
			toolName: "sys",
			content: [{ type: "text", text: "进程检查完成" }],
			details: { process: { byCpu: Array.from({ length: 80 }, (_, i) => ({ name: `process-${i}`, cpuPct: i })) } },
			timestamp: now + 2,
		},
		{
			role: "assistant",
			content: [{ type: "text", text: "已完成进程检查。" }],
			timestamp: now + 3,
			stopReason: "stop",
		},
	];
	const clients = new Set<ServerResponse>();
	const port = await freePort();
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions, stage: "process-test" }),
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
			if (path === "/api/sessions/history")
				data = {
					id: "history",
					messages,
					entries: messages.map((message, i) => ({ id: `entry-${i}`, message })),
					running: false,
					queue: { version: 0, items: [], paused: false },
					ui: [],
				};
			else if (path === "/api/sessions") data = { sessions };
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
		await browser.navigate(`http://127.0.0.1:${port}/s/history`);
		await browser.until("!!document.querySelector('.turn-process-head')", "历史过程入口");
		assert.equal(
			await browser.evaluate("document.querySelector('.turn-process-head').getAttribute('aria-expanded')"),
			"false",
		);
		assert.equal(
			await browser.evaluate("document.querySelectorAll('.turn-process-body, .tool-listing li').length"),
			0,
			"未展开的历史过程不创建内容节点",
		);
		const heights = await browser.evaluate<number[]>(`(async () => {
			const trigger = document.querySelector('.turn-process-head');
			const panel = trigger.closest('.turn-process').querySelector('.disclosure__content');
			const heights = [panel.getBoundingClientRect().height]; trigger.click();
			const start = performance.now();
			while (performance.now()-start<350) { await new Promise(r=>requestAnimationFrame(r)); heights.push(panel.getBoundingClientRect().height); }
			return heights;
		})()`);
		await browser.until("!!document.querySelector('.tool-listing li')", "首次展开挂载完整工具内容");
		assert.equal(await browser.evaluate("document.querySelectorAll('.tool-listing li').length"), 40);
		assert.ok(
			heights.some((h) => h > 1 && h < Math.max(...heights) - 1),
			"首次展开包含中间高度帧",
		);
		await browser.click(".tool-group-heading");
		await browser.click(".tool-card-heading");
		await browser.click(".tool-listing button");
		await browser.until("document.querySelectorAll('.tool-listing li').length===80", "展开剩余排行");
		await browser.evaluate(
			"window.__processRow = document.querySelector('.tool-listing li'); window.__processBody = document.querySelector('.turn-process-body'); void 0",
		);
		await browser.click(".turn-process-head");
		await browser.until(
			"document.querySelector('.turn-process-head').getAttribute('aria-expanded')==='false'",
			"收起过程",
		);
		assert.equal(
			await browser.evaluate("window.__processRow.isConnected && window.__processBody.isConnected"),
			true,
			"收起保留已挂载节点",
		);
		await browser.click(".turn-process-head");
		await browser.until(
			"document.querySelector('.turn-process-head').getAttribute('aria-expanded')==='true'",
			"再次展开过程",
		);
		assert.equal(
			await browser.evaluate(
				"document.querySelector('.tool-listing li')===window.__processRow && document.querySelector('.turn-process-body')===window.__processBody",
			),
			true,
			"再次展开复用节点",
		);
		assert.equal(await browser.evaluate("document.querySelectorAll('.tool-listing li').length"), 80, "保留分页状态");
		assert.equal(
			await browser.evaluate("document.querySelector('.tool-card-heading').getAttribute('aria-expanded')"),
			"true",
			"保留工具展开状态",
		);
		await writeFile(join(root, "evidence.json"), JSON.stringify({ heights, retainedRows: 80 }, null, 2));
		console.log("PROCESS_EVIDENCE", root);
	} finally {
		await browser?.close();
		for (const client of clients) client.end();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	}
});
