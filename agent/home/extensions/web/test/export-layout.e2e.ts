import assert from "node:assert/strict";
import { mkdtemp, writeFile } from "node:fs/promises";
import type { ServerResponse } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { exportOptions, exportSnapshot } from "../server/exports/snapshot.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import type { ConversationEntry, Message } from "../shared/conversation.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

const messages: Message[] = [
	{ role: "user", content: "检查进程并说明结论", timestamp: 1 },
	{
		role: "assistant",
		timestamp: 2,
		stopReason: "toolUse",
		content: [
			{ type: "thinking", thinking: "先核对进程信息，再检查输出。\n".repeat(4) },
			{ type: "text", text: "正在核对进程信息。" },
			{ type: "toolCall", id: "read", name: "read", arguments: { path: "process.txt" } },
		],
	},
	{
		role: "toolResult",
		timestamp: 3,
		toolCallId: "read",
		toolName: "read",
		content: [{ type: "text", text: "进程输出内容\n".repeat(4) }],
	},
	{
		role: "assistant",
		timestamp: 4,
		stopReason: "stop",
		content: [{ type: "text", text: "## 检查结论\n\n进程信息已确认。" }],
	},
];
const entries: ConversationEntry[] = messages.map((message, index) => ({ id: `entry-${index}`, message }));
const measure = `(() => {
	const style = (selector, pseudo) => {
		const node = document.querySelector(selector);
		if (!node) return null;
		const css = getComputedStyle(node, pseudo);
		return Object.fromEntries(['display','flexDirection','gap','paddingTop','paddingLeft','marginLeft','marginTop','width','backgroundColor','fontSize','lineHeight'].map(key=>[key,css[key]]));
	};
	const root = document.querySelector('.assistant-turn');
	const body = document.querySelector('.turn-process-body');
	const group = document.querySelector('.tool-group > .disclosure__content');
	return {
		assistantCount: document.querySelectorAll('.assistant-block').length,
		turn: style('.turn'), assistant: style('.assistant-turn'), body: style('.turn-process-body'),
		group: style('.tool-group > .disclosure__content'), guide: style('.tool-group > .disclosure__content','::before'),
		detail: style('.tool-card-detail'), detailGuide: style('.tool-card-detail','::before'),
		thinking: style('.thinking-line .tool-text'),
		bodyChildren: body ? [...body.children].map(e=>e.className) : [],
		answerOutsideProcess: !!root?.querySelector(':scope > .markdown'),
		groupIndent: root && group ? group.getBoundingClientRect().left-root.getBoundingClientRect().left+parseFloat(getComputedStyle(group).paddingLeft) : null,
		rawIndicatorOffset: (()=>{const head=document.querySelector('.tool-raw-toggle'),icon=head?.querySelector('.disclosure__indicator');return head&&icon ? icon.getBoundingClientRect().left-head.getBoundingClientRect().left : null;})(),
	};
})()`;

test("导出与聊天使用同一轮次布局、间距和引导线，HTML 展开包含连续中间帧", { timeout: 45_000 }, async () => {
	const root = await mkdtemp(join(await testRoot(), "export-layout-"));
	const port = await freePort();
	const origin = `http://127.0.0.1:${port}`;
	const sessions = [{ id: "fixture", title: "布局对照", running: false, updatedAt: new Date().toISOString() }];
	const clients = new Set<ServerResponse>();
	const server = createWebServer(
		fileURLToPath(new URL("../static/", import.meta.url)),
		port,
		() => ({ sessions, stage: "test" }),
		async (request, response, path) => {
			if (!path.startsWith("/api/")) return false;
			if (path.endsWith("/events")) {
				response.writeHead(200, { "Content-Type": "text/event-stream" });
				response.write(": connected\n\n");
				clients.add(response);
				response.on("close", () => clients.delete(response));
				return true;
			}
			let data: unknown = {};
			if (path === "/api/sessions/fixture")
				data = {
					id: "fixture",
					entries,
					messages,
					running: false,
					queue: { version: 0, items: [], paused: false },
					ui: [],
				};
			else if (path === "/api/sessions/fixture/exports/preview") {
				const chunks: Buffer[] = [];
				for await (const chunk of request) chunks.push(chunk);
				data = exportSnapshot(
					"fixture",
					"布局对照",
					entries,
					exportOptions(JSON.parse(Buffer.concat(chunks).toString())),
				);
			} else if (path === "/api/sessions") data = { sessions };
			else if (path === "/api/models") data = { models: [], selected: null, enabled: null, thinkingLevels: [] };
			else if (path === "/api/settings") data = { theme: "light", providers: [], models: [], scopedModels: [] };
			else if (path === "/api/auth") data = { providers: [] };
			response.writeHead(200, { "Content-Type": "application/json", Connection: "close" });
			response.end(JSON.stringify(data));
			return true;
		},
	);
	await listenWebServer(server, port);
	let browser: BrowserProbe | undefined;
	try {
		browser = await BrowserProbe.launch(root);
		await browser.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "no-preference" }],
		});
		await browser.navigate(`${origin}/s/fixture`);
		await browser.until("!!document.querySelector('.turn-process-head')", "聊天过程就绪");
		for (const selector of [".turn-process-head", ".thinking-trigger", ".tool-group-heading", ".tool-card-heading"])
			await browser.click(selector);
		await browser.evaluate("new Promise(r=>setTimeout(r,300))");
		const chat = await browser.evaluate<Record<string, unknown>>(measure);
		await browser.screenshot(join(root, "chat.png"));
		await browser.call("Page.navigate", { url: `${origin}/_export` });
		await browser.until("document.documentElement.dataset.exportReceiver==='ready'", "导出接收就绪");
		const snapshot = exportSnapshot(
			"fixture",
			"布局对照",
			entries,
			exportOptions({ format: "html", includeThinking: true, includeTools: true }),
		);
		await browser.evaluate(
			`window.postMessage({ type:'cst-export-preview', snapshot:${JSON.stringify(snapshot)} },location.origin)`,
		);
		await browser.until("document.documentElement.dataset.exportState==='ready'", "导出内容就绪");
		for (const selector of [".turn-process-head", ".thinking-trigger", ".tool-group-heading", ".tool-card-heading"])
			await browser.evaluate(`document.querySelector(${JSON.stringify(selector)})?.click()`);
		await browser.evaluate("new Promise(r=>setTimeout(r,300))");
		const exported = await browser.evaluate<Record<string, unknown>>(measure);
		await browser.screenshot(join(root, "export.png"));
		await writeFile(join(root, "measurements.json"), JSON.stringify({ chat, exported }, null, 2));
		console.info("EXPORT_LAYOUT_EVIDENCE", root);
		assert.equal(exported.assistantCount, chat.assistantCount, "同一轮工具前后的助手消息合并呈现");
		assert.ok(exported.body, "导出保留聊天页的过程布局容器");
		for (const key of ["turn", "assistant", "body", "group", "guide", "detail", "detailGuide", "thinking"]) {
			const withoutWidth = (value: unknown) => {
				const copy = { ...(value as Record<string, unknown>) };
				if (key !== "guide" && key !== "detailGuide") delete copy.width;
				if (key === "turn") delete copy.marginLeft;
				return copy;
			};
			assert.deepEqual(withoutWidth(exported[key]), withoutWidth(chat[key]), `${key} 沿用聊天页间距与引导线样式`);
		}
		assert.equal(exported.answerOutsideProcess, true);
		assert.equal(exported.groupIndent, chat.groupIndent);
		assert.equal(exported.rawIndicatorOffset, chat.rawIndicatorOffset, "原始数据箭头与聊天页位置一致");
		const motion: Record<string, number[]> = {};
		for (const selector of [".thinking-line", ".tool-group", ".turn-process"]) {
			const heights = await browser.evaluate<number[]>(`(async()=>{
				const node=document.querySelector(${JSON.stringify(selector)});node.querySelector('summary').click();
				await new Promise(r=>setTimeout(r,300));
				const heights=[node.getBoundingClientRect().height];node.querySelector('summary').click();
				const start=performance.now();while(performance.now()-start<350){await new Promise(r=>requestAnimationFrame(r));heights.push(node.getBoundingClientRect().height);}return heights;
			})()`);
			motion[selector] = heights;
			assert.ok(
				heights.some((height) => height > Math.min(...heights) + 1 && height < Math.max(...heights) - 1),
				`${selector} 展开高度连续变化`,
			);
		}
		await writeFile(join(root, "motion.json"), JSON.stringify(motion, null, 2));
		await browser.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "reduce" }],
		});
		await browser.click(".thinking-trigger");
		assert.equal(
			await browser.evaluate(
				"document.querySelector('.thinking-line').getAnimations({subtree:true}).some(a=>a.playState==='running')",
			),
			false,
			"减少动态时直接收起",
		);
		await browser.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "no-preference" }],
		});
		await browser.navigate(`${origin}/s/fixture/export`);
		await browser.click(".export-check:nth-of-type(1) input");
		await browser.click(".export-check:nth-of-type(2) input");
		await browser.until(
			"document.querySelectorAll('iframe').length===1 && document.querySelector('iframe')?.contentDocument?.querySelector('.thinking-line') && document.querySelector('iframe')?.contentDocument?.querySelector('.tool-group') && document.querySelector('iframe')?.dataset.ready==='true'",
			"默认 PNG 预览就绪",
		);
		const previewMotion: Record<string, { heights: number[]; opacities: number[]; rotations: string[] }> = {};
		for (const selector of [".thinking-line", ".tool-group", ".turn-process"]) {
			const samples = await browser.evaluate<{
				heights: number[];
				opacities: number[];
				rotations: string[];
			}>(`(async()=>{
				const frame=document.querySelector('iframe'),view=frame.contentWindow;
				const node=frame.contentDocument.querySelector(${JSON.stringify(selector)});
				const indicator=node.querySelector('summary > .disclosure__indicator');
				node.querySelector('summary').click();await new Promise(r=>setTimeout(r,300));
				const samples={heights:[],opacities:[],rotations:[]};
				const sample=()=>{samples.heights.push(node.getBoundingClientRect().height);samples.opacities.push(Number(view.getComputedStyle(node,'::details-content').opacity));samples.rotations.push(view.getComputedStyle(indicator).transform);};
				sample();node.querySelector('summary').click();
				const start=performance.now();while(performance.now()-start<350){await new Promise(r=>requestAnimationFrame(r));sample();}return samples;
			})()`);
			previewMotion[selector] = samples;
			assert.ok(
				samples.heights.some(
					(height) => height > Math.min(...samples.heights) + 1 && height < Math.max(...samples.heights) - 1,
				),
				`默认 PNG 预览的 ${selector} 高度连续变化`,
			);
			assert.ok(
				samples.opacities.some((opacity) => opacity > 0 && opacity < 1),
				`默认 PNG 预览的 ${selector} 内容淡入`,
			);
			assert.ok(
				samples.rotations.some(
					(rotation) => rotation !== samples.rotations[0] && rotation !== samples.rotations.at(-1),
				),
				`默认 PNG 预览的 ${selector} 箭头连续转动`,
			);
		}
		await writeFile(join(root, "preview-motion.json"), JSON.stringify(previewMotion, null, 2));
	} finally {
		await browser?.close();
		for (const client of clients) client.end();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	}
});
