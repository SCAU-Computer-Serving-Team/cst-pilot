import assert from "node:assert/strict";
import { mkdtemp, writeFile } from "node:fs/promises";
import type { ServerResponse } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";
import { captureExportDocument } from "../server/exports/document.ts";
import { OfflineHtml } from "../server/exports/html.ts";
import { exportOptions, exportSnapshot } from "../server/exports/snapshot.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import type { ContentPart, ConversationEntry } from "../shared/conversation.ts";
import type { ExportDocumentSource } from "../shared/exports.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

const calls: Extract<ContentPart, { type: "toolCall" }>[] = [
	{ type: "toolCall", id: "overview", name: "sys", arguments: { scope: "overview" } },
	{ type: "toolCall", id: "plan", name: "command_plan", arguments: { title: "检查系统文件" } },
	{ type: "toolCall", id: "unlock", name: "bitlocker", arguments: { scope: "unlock", drive: "D:" } },
	{ type: "toolCall", id: "search", name: "web_search", arguments: { queries: ["Windows 磁盘诊断", "系统文件检查"] } },
];
const results = [
	{
		overview: {
			cpuTotalPct: 18,
			logicalCores: 8,
			mem: { usedPct: 42, totalMB: 16384 },
			machine: { vendor: "CST", cpu: "测试处理器" },
			uptime: { days: 1, hours: 2, minutes: 3 },
		},
	},
	{
		runbook: {
			title: "检查系统文件",
			action: "planned",
			version: 1,
			items: 1,
			levelLabel: "安全",
			commands: [{ shell: "cmd", admin: true, summary: "只读检查系统文件", command: "sfc /verifyonly" }],
		},
	},
	{
		bitlocker: {
			volumes: [
				{
					volumeId: "fixture-volume",
					drive: "D:",
					conversionStatus: 1,
					lockStatus: 0,
					encryptionPercentage: 100,
					operation: { status: "accepted" },
				},
			],
		},
	},
	{ queryCount: 2, successfulQueries: 2, totalResults: 3 },
];
const entries: ConversationEntry[] = [
	{ id: "user", message: { role: "user", content: "检查电脑并整理方案", timestamp: 1 } },
	{ id: "assistant", message: { role: "assistant", content: calls, timestamp: 2, stopReason: "toolUse" } },
	...calls.map(
		(call, index) =>
			({
				id: `${call.id}-result`,
				message: {
					role: "toolResult",
					toolCallId: call.id,
					toolName: call.name,
					content: [{ type: "text", text: "诊断完成" }],
					details: results[index],
					timestamp: index + 3,
				},
			}) satisfies ConversationEntry,
	),
];
const measure = `(() => {
	const props=['display','backgroundColor','boxShadow','borderTopColor','borderTopWidth','borderRadius','paddingTop','paddingRight','paddingBottom','paddingLeft','gap','fontFamily','fontSize','fontWeight','lineHeight','color'];
	const style=node=>node ? Object.fromEntries(props.map(key=>[key,getComputedStyle(node)[key]])) : null;
	return [...document.querySelectorAll('.special-tool')].map(card=>({
		id:card.dataset.toolCallId, card:style(card),
		heading:style(card.querySelector('.special-tool-heading,.special-tool-search-trigger')),
		title:style(card.querySelector('h3,.special-tool-search-trigger > span:not(.disclosure__indicator)')),
		footer:style(card.querySelector('.special-tool-footer')),
		headingVisible:card.querySelector('.special-tool-heading,.special-tool-search-trigger')?.checkVisibility(),
		detailVisible:card.querySelector('.special-tool-detail-trigger')?.checkVisibility() ?? null,
		footerHeight:card.querySelector('.special-tool-footer')?.getBoundingClientRect().height ?? null,
	}));
})()`;

test("特殊工具在聊天、PNG、HTML 与离线文档中共用外观和可读折叠", { timeout: 60_000 }, async () => {
	const root = await mkdtemp(join(await testRoot(), "export-special-tools-"));
	const clients = new Set<ServerResponse>();
	const port = await freePort();
	const origin = `http://127.0.0.1:${port}`;
	const sessions = [{ id: "fixture", title: "特殊工具外观对照", running: false, updatedAt: new Date().toISOString() }];
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
				response.on("close", () => clients.delete(response));
				return true;
			}
			let data: unknown = {};
			if (path === "/api/sessions/fixture")
				data = {
					id: "fixture",
					entries,
					messages: entries.map((entry) => entry.message),
					running: false,
					queue: { version: 0, items: [], paused: false },
					ui: [],
				};
			else if (path === "/api/sessions") data = { sessions };
			else if (path === "/api/models") data = { models: [], selected: null, enabled: null, thinkingLevels: [] };
			else if (path === "/api/settings") data = { theme: "light", providers: [], models: [], scopedModels: [] };
			else if (path === "/api/auth") data = { providers: [] };
			response.writeHead(200, { "Content-Type": "application/json" });
			response.end(JSON.stringify(data));
			return true;
		},
	);
	await listenWebServer(server, port);
	const pack = new OfflineHtml(origin);
	let browser: BrowserProbe | undefined;
	try {
		browser = await BrowserProbe.launch(root);
		await browser.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "reduce" }],
		});
		for (const theme of ["light", "dark"] as const) {
			await browser.navigate(`${origin}/s/fixture`);
			await browser.until("document.querySelectorAll('.special-tool').length===4", "聊天特殊工具就绪");
			await browser.evaluate(`document.documentElement.dataset.theme=${JSON.stringify(theme)}`);
			await browser.evaluate("document.fonts.ready");
			const chat: Record<string, unknown>[] = await browser.evaluate(measure);
			await browser.screenshot(join(root, `chat-${theme}.png`));
			for (const format of ["png", "html"] as const) {
				await browser.call("Page.navigate", { url: `${origin}/_export` });
				await browser.until("document.documentElement.dataset.exportReceiver==='ready'", "导出接收就绪");
				const snapshot = exportSnapshot(
					"fixture",
					"特殊工具外观对照",
					entries,
					exportOptions({ format, theme, includeTools: true }),
				);
				await browser.evaluate(
					`window.postMessage({type:'cst-export-preview',snapshot:${JSON.stringify(snapshot)}},location.origin)`,
				);
				await browser.until("document.documentElement.dataset.exportState==='ready'", "导出内容就绪");
				const exported: Record<string, unknown>[] = await browser.evaluate(measure);
				await writeFile(join(root, `${format}-${theme}.json`), JSON.stringify({ chat, exported }, null, 2));
				await browser.screenshot(join(root, `${format}-${theme}.png`));
				assert.deepEqual(exported, chat, `${format} ${theme} 卡片、标题、阴影、字号和详情入口与聊天一致`);
				assert.equal(
					await browser.evaluate("document.querySelectorAll('.special-tool details[open]').length"),
					0,
					"详情与检索关键词初始折叠",
				);
				if (format === "html") {
					const source = await browser.evaluate<ExportDocumentSource>(`(${captureExportDocument.toString()})()`);
					const file = join(root, `offline-${theme}.html`);
					await writeFile(file, await pack.render(snapshot, source, AbortSignal.timeout(10_000)));
					await browser.call("Page.navigate", { url: pathToFileURL(file).href });
					await browser.until("location.protocol==='file:' && document.readyState==='complete'", "离线导出就绪");
					await browser.evaluate("document.fonts.ready");
					assert.deepEqual(await browser.evaluate(measure), chat, "离线文档保留完整卡片外观");
					assert.equal(
						await browser.evaluate("document.querySelectorAll('button,input,script').length"),
						0,
						"离线文档无提交与复制操作",
					);
				}
				await browser.click(".special-tool-search-trigger");
				assert.equal(
					await browser.evaluate(
						"document.querySelector('.special-tool-search .disclosure__content p').checkVisibility()",
					),
					true,
					"检索关键词可展开",
				);
				await browser.click(".special-tool-detail-trigger");
				assert.equal(
					await browser.evaluate("document.querySelector('.special-tool-invocation').checkVisibility()"),
					true,
					"工具详情可展开",
				);
				assert.equal(await browser.evaluate("document.documentElement.scrollWidth<=innerWidth"), true);
			}
		}
		console.info("EXPORT_SPECIAL_TOOLS_EVIDENCE", root);
	} finally {
		await browser?.close();
		for (const client of clients) client.end();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	}
});
