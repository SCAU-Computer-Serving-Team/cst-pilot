import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import type { ServerResponse } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createWebServer, listenWebServer } from "../server/http.ts";
import type { ContentPart, ConversationEntry, Message } from "../shared/conversation.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

const pixel = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9ZQmcAAAAASUVORK5CYII=";

test("特殊工具后续过程：无零次计数、内容保留、普通工具计数、图片与流式连续性", { timeout: 45_000 }, async () => {
	const root = join(await testRoot(), "special-tool-process");
	await mkdir(root, { recursive: true });
	const entries: ConversationEntry[] = [];
	let stamp = Date.now() - 60_000;
	const append = (message: Omit<Message, "timestamp">) => {
		const entry = { id: `entry-${entries.length}`, message: { ...message, timestamp: stamp++ } };
		entries.push(entry);
		return entry;
	};
	const call = (name: string, id: string, args: Record<string, unknown> = {}): ContentPart => ({
		type: "toolCall",
		name,
		id,
		arguments: args,
	});
	const result = (name: string, id: string) =>
		append({
			role: "toolResult",
			toolCallId: id,
			toolName: name,
			content: [{ type: "text", text: "检查完成" }],
			details: name === "web_search" ? { queryCount: 2, successfulQueries: 2, totalResults: 0 } : {},
		});
	const matrix: [string, Record<string, unknown>][] = [
		["web_search", { queries: ["关键词一", "关键词二"] }],
		["sys", {}],
		["sys", { scope: "overview" }],
		["runbook", {}],
		["command_plan", {}],
		["runbook_deliver", {}],
		["bitlocker", { scope: "unlock", drive: "D:" }],
		["bitlocker", { scope: "decrypt" }],
	];
	for (const [index, [name, args]] of matrix.entries()) {
		append({ role: "user", content: `检查 ${name}` });
		append({
			role: "assistant",
			stopReason: "toolUse",
			content: [{ type: "thinking", thinking: "先检查相关信息。" }, call(name, `special-${index}`, args)],
		});
		result(name, `special-${index}`);
		append({
			role: "assistant",
			stopReason: "toolUse",
			content:
				index % 2
					? [{ type: "text", text: `后续说明 ${index}` }]
					: [{ type: "thinking", thinking: `后续思考 ${index}` }],
		});
		append({ role: "assistant", stopReason: "stop", content: "检查已结束。" });
	}
	append({ role: "user", content: "混合普通工具与特殊工具" });
	append({
		role: "assistant",
		stopReason: "toolUse",
		content: [call("read", "before"), call("web_search", "mixed"), call("read", "after")],
	});
	for (const [name, id] of [
		["read", "before"],
		["web_search", "mixed"],
		["read", "after"],
	])
		result(name, id);
	append({ role: "assistant", stopReason: "stop", content: "混合检查结束。" });
	append({ role: "user", content: "联网后读取图片" });
	append({
		role: "assistant",
		stopReason: "toolUse",
		content: [call("web_search", "image-search"), call("read", "image-read", { path: "fixture.png" })],
	});
	result("web_search", "image-search");
	append({
		role: "toolResult",
		toolCallId: "image-read",
		toolName: "read",
		content: [{ type: "image", data: pixel, mimeType: "image/png" }],
	});
	append({ role: "assistant", stopReason: "stop", content: "图片已读取。" });
	append({ role: "user", content: "联网后普通检查中断" });
	append({
		role: "assistant",
		stopReason: "aborted",
		content: [call("web_search", "interrupted-search"), call("read", "interrupted-read")],
	});
	result("web_search", "interrupted-search");
	append({ role: "user", content: "等待联网后的思考输出" });
	append({ role: "assistant", stopReason: "toolUse", content: [call("web_search", "live-search")] });
	result("web_search", "live-search");
	const streaming = append({ role: "assistant", content: [{ type: "thinking", thinking: "" }] });
	let running = true;
	const clients = new Set<ServerResponse>();
	const sessionClients = new Set<ServerResponse>();
	const port = await freePort();
	const origin = `http://127.0.0.1:${port}`;
	const sessions = [{ id: "special", title: "特殊工具过程", updatedAt: new Date().toISOString(), running }];
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions, stage: "test" }),
		async (_request, response, path) => {
			if (!path.startsWith("/api/")) return false;
			if (path.endsWith("/events")) {
				response.writeHead(200, { "Content-Type": "text/event-stream" });
				response.write(": connected\n\n");
				clients.add(response);
				if (path === "/api/sessions/special/events") sessionClients.add(response);
				response.on("close", () => {
					clients.delete(response);
					sessionClients.delete(response);
				});
				return true;
			}
			let data: unknown = {};
			if (path === "/api/sessions/special")
				data = {
					id: "special",
					entries,
					messages: entries.map((entry) => entry.message),
					running,
					queue: { version: 0, items: [], paused: false },
					ui: [],
				};
			else if (path === "/api/sessions") data = { sessions };
			else if (path === "/api/models") data = { models: [], selected: null, enabled: null, thinkingLevels: [] };
			else if (path === "/api/settings") data = { theme: "light", providers: [], models: [], scopedModels: [] };
			else if (path === "/api/auth") data = { providers: [] };
			response.writeHead(200, { "Content-Type": "application/json", Connection: "close" });
			response.end(JSON.stringify(data));
			return true;
		},
	);
	await listenWebServer(server, port);
	const emit = (change: unknown) => {
		for (const client of sessionClients) client.write(`event: session\ndata: ${JSON.stringify(change)}\n\n`);
	};
	let browser: BrowserProbe | undefined;
	try {
		browser = await BrowserProbe.launch(root);
		await browser.navigate(`${origin}/s/special`);
		await browser.until("document.querySelectorAll('.assistant-turn').length===12", "全部特殊工具轮次");
		assert.equal(
			await browser.evaluate(
				"[...document.querySelectorAll('.turn-process-head,.tool-group-heading')].some(e=>/(?:已完成|已调用)\\s*0\\s*(?:次|个)/.test(e.textContent))",
			),
			false,
		);
		for (let index = 0; index < matrix.length; index++) {
			assert.equal(
				await browser.evaluate(`!!document.querySelector('[data-tool-call-id="special-${index}"]')`),
				true,
				`${matrix[index][0]} 独立组件可见`,
			);
			const label: string = await browser.evaluate<string>(
				`document.querySelectorAll('.assistant-turn')[${index}].querySelectorAll('.turn-process-head')[1].textContent.trim()`,
			);
			assert.equal(label, index % 2 ? "处理过程" : "思考过程");
			await browser.evaluate(
				`document.querySelectorAll('.assistant-turn')[${index}].querySelectorAll('.turn-process-head')[1].click()`,
			);
			if (!(index % 2))
				await browser.evaluate(
					`document.querySelectorAll('.assistant-turn')[${index}].querySelector('.thinking-trigger').click()`,
				);
			assert.equal(
				await browser.evaluate(
					`document.querySelectorAll('.assistant-turn')[${index}].textContent.includes(${JSON.stringify(index % 2 ? `后续说明 ${index}` : `后续思考 ${index}`)})`,
				),
				true,
			);
		}
		assert.equal(
			await browser.evaluate(
				"document.querySelectorAll('.assistant-turn')[8].querySelectorAll('.turn-process-head')[1].textContent.trim()",
			),
			"已完成 1 次工具调用",
		);
		assert.equal(
			await browser.evaluate(
				"document.querySelectorAll('.assistant-turn')[9].querySelectorAll('.turn-process-head')[1].textContent.trim()",
			),
			"处理过程",
		);
		await browser.evaluate(
			"document.querySelectorAll('.assistant-turn')[9].querySelectorAll('.turn-process-head')[1].click()",
		);
		await browser.until("document.querySelector('.tool-image')?.naturalWidth>0", "独立读取图片未丢失");
		assert.equal(
			await browser.evaluate(
				"document.querySelectorAll('.assistant-turn')[10].querySelectorAll('.turn-process-head')[1].textContent.trim()",
			),
			"检查未完成",
		);
		assert.equal(
			await browser.evaluate(
				"document.querySelectorAll('.assistant-turn')[11].querySelectorAll('.turn-process-head').length",
			),
			1,
			"空的后续过程不创建入口",
		);
		await browser.evaluate(
			"window.__workHead=document.querySelectorAll('.assistant-turn')[11].querySelector('.turn-process-head');void 0",
		);
		streaming.message.content = [{ type: "thinking", thinking: "实时思考内容" }];
		emit({ type: "message_update", message: streaming.message });
		await browser.until(
			"document.querySelectorAll('.assistant-turn')[11].querySelectorAll('.turn-process-head').length===2",
			"流式思考过程入口出现",
		);
		await browser.evaluate(
			"document.querySelectorAll('.assistant-turn')[11].querySelector('.thinking-trigger').click()",
		);
		await browser.until(
			"document.querySelectorAll('.assistant-turn')[11].textContent.includes('实时思考内容')",
			"联网后流式思考保留且立即显示",
		);
		await browser.evaluate(
			"document.querySelectorAll('.assistant-turn')[11].querySelectorAll('.turn-process-head')[1].click()",
		);
		streaming.message.content = [
			{ type: "thinking", thinking: "实时思考内容继续追加" },
			{ type: "text", text: "实时回答已结束。" },
		];
		streaming.message.stopReason = "stop";
		running = false;
		emit({ type: "message_end", message: streaming.message });
		emit({ type: "agent_end" });
		await browser.until(
			"document.querySelectorAll('.assistant-turn')[11].textContent.includes('实时回答已结束。')",
			"思考后的最终回答",
		);
		assert.equal(await browser.evaluate("window.__workHead.isConnected"), true, "首段工作时长入口保持挂载");
		assert.equal(
			await browser.evaluate(
				"document.querySelectorAll('.assistant-turn')[11].querySelectorAll('.turn-process-head')[1].getAttribute('aria-expanded')",
			),
			"false",
			"保留用户收起选择",
		);
		await browser.evaluate("document.querySelector('.turn-process-head').click()");
		await browser.until(
			"document.querySelectorAll('.assistant-turn')[0].querySelectorAll('.thinking-trigger').length===2",
			"首段与后续思考均已挂载",
		);
		await browser.evaluate(
			"document.querySelectorAll('.assistant-turn')[0].querySelectorAll('.thinking-trigger').forEach(e=>{if(e.getAttribute('aria-expanded')!=='true')e.click()});document.querySelector('.assistant-turn').scrollIntoView({block:'start'});",
		);
		await browser.until(
			"document.querySelectorAll('.assistant-turn')[0].textContent.includes('先检查相关信息。')",
			"思考原文完整展示",
		);
		await browser.evaluate("document.fonts.ready");
		await browser.evaluate(
			"Promise.all(document.querySelectorAll('.assistant-turn')[0].getAnimations({subtree:true}).filter(a=>a.effect?.getTiming().iterations!==Infinity).map(a=>a.finished.catch(()=>{})))",
		);
		await browser.screenshot(join(root, "light.png"));
		await browser.evaluate("document.documentElement.dataset.theme='dark'");
		await browser.screenshot(join(root, "dark.png"));
		await browser.call("Emulation.setDeviceMetricsOverride", {
			width: 390,
			height: 844,
			deviceScaleFactor: 1,
			mobile: false,
		});
		assert.equal(await browser.evaluate("document.documentElement.scrollWidth<=innerWidth"), true);
		await browser.screenshot(join(root, "narrow.png"));
		await browser.navigate(`${origin}/s/special`);
		await browser.until("document.querySelectorAll('.assistant-turn').length===12", "刷新后恢复");
		assert.equal(
			await browser.evaluate(
				"[...document.querySelectorAll('.turn-process-head,.tool-group-heading')].some(e=>/(?:已完成|已调用)\\s*0\\s*(?:次|个)/.test(e.textContent))",
			),
			false,
		);
	} finally {
		await browser?.close();
		for (const client of clients) client.end();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	}
});
