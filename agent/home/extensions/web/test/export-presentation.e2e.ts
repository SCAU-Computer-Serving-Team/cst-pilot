import assert from "node:assert/strict";
import { mkdtemp, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";
import { captureExportDocument } from "../server/exports/document.ts";
import { OfflineHtml } from "../server/exports/html.ts";
import { exportOptions, exportSnapshot } from "../server/exports/snapshot.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import type { ConversationEntry } from "../shared/conversation.ts";
import type { ExportDocumentSource } from "../shared/exports.ts";
import { BrowserProbe, freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

const image = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a3foAAAAASUVORK5CYII=";
const entries: ConversationEntry[] = [
	{ id: "user", message: { role: "user", content: "检查磁盘与内存", timestamp: 1 } },
	{
		id: "assistant",
		message: {
			role: "assistant",
			timestamp: 2,
			content: [
				{ type: "thinking", thinking: "先检查磁盘，再确认内存。Thinking fixture." },
				{ type: "toolCall", id: "disk", name: "read", arguments: { path: "disk.txt" } },
				{ type: "toolCall", id: "memory", name: "read", arguments: { path: "memory.txt" } },
				{ type: "toolCall", id: "image", name: "read", arguments: { path: "image.png" } },
				{ type: "image", mimeType: "image/png", data: image },
				{ type: "text", text: "## 检查结论\n\n磁盘空间充足，内存检查未完成。" },
			],
		},
	},
	{
		id: "disk-result",
		message: {
			role: "toolResult",
			toolCallId: "disk",
			toolName: "read",
			content: [{ type: "text", text: "磁盘详细结果。Disk fixture output." }],
			timestamp: 3,
		},
	},
	{
		id: "image-result",
		message: {
			role: "toolResult",
			toolCallId: "image",
			toolName: "read",
			content: [{ type: "image", mimeType: "image/png", data: image }],
			timestamp: 4,
		},
	},
];

test("导出沿用聊天思考与工具组件：PNG 展开一层，HTML 离线两层折叠与字体图片完整", { timeout: 60_000 }, async () => {
	const root = await mkdtemp(join(await testRoot(), "export-presentation-"));
	const port = await freePort();
	const origin = `http://127.0.0.1:${port}`;
	const server = createWebServer(fileURLToPath(new URL("../static/", import.meta.url)), port);
	await listenWebServer(server, port);
	const pack = new OfflineHtml(origin);
	let browser: BrowserProbe | undefined;
	try {
		browser = await BrowserProbe.launch(root);
		await browser.call("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "no-preference" }],
		});
		for (const theme of ["light", "dark"] as const) {
			for (const format of ["png", "html"] as const) {
				await browser.call("Page.navigate", { url: `${origin}/_export` });
				await browser.until("document.documentElement.dataset.exportReceiver==='ready'", "只读页接收就绪");
				const snapshot = exportSnapshot(
					"fixture",
					"思考与工具调用结果",
					entries,
					exportOptions({ format, theme, includeThinking: true, includeTools: true }),
				);
				await browser.evaluate(
					`window.postMessage({ type: 'cst-export-preview', snapshot: ${JSON.stringify(snapshot)} }, location.origin)`,
				);
				await browser.until("document.documentElement.dataset.exportState==='ready'", "内容与字体就绪");
				assert.equal(await browser.evaluate("document.querySelector('.turn-process').open"), format === "png");
				assert.equal(await browser.evaluate("document.querySelector('.thinking-line').open"), format === "png");
				assert.equal(await browser.evaluate("document.querySelector('.tool-group').open"), format === "png");
				assert.equal(await browser.evaluate("document.querySelectorAll('.tool-group .tool-card').length"), 2);
				assert.equal(
					await browser.evaluate("document.querySelectorAll('.tool-group details[open]').length"),
					0,
					"工具详情与原始结果均保持折叠",
				);
				assert.match(
					await browser.evaluate<string>("document.querySelector('.tool-group-heading').textContent"),
					/检查未完成.*2 次工具调用/,
				);
				assert.equal(
					await browser.evaluate(
						"[...document.querySelectorAll('.tool-group .tool-card-heading')].some(e=>e.textContent.includes('未完成或已中断'))",
					),
					true,
				);
				assert.equal(
					await browser.evaluate("document.querySelector('.tool-group .tool-card-heading').checkVisibility()"),
					format === "png",
				);
				assert.equal(
					await browser.evaluate("document.querySelector('.tool-group .tool-invocation').checkVisibility()"),
					false,
				);
				assert.equal(
					await browser.evaluate("document.querySelector('.thinking-line pre').checkVisibility()"),
					format === "png",
				);
				assert.equal(
					await browser.evaluate("document.querySelector('.tool-card--special img').checkVisibility()"),
					format === "png",
					"独立图片沿用聊天页的过程层级",
				);
				assert.equal(
					await browser.evaluate("document.querySelector('.turn-visible img').checkVisibility()"),
					true,
					"助手附图保留在过程外",
				);
				assert.equal(
					await browser.evaluate("document.querySelector('.turn-visible img').getBoundingClientRect().width"),
					1,
					"小图保留原尺寸",
				);
				assert.equal(await browser.evaluate("document.documentElement.scrollWidth<=innerWidth"), true);
				await browser.screenshot(join(root, `${format}-${theme}.png`));
				if (format === "png") {
					assert.equal(
						await browser.evaluate(
							"getComputedStyle(document.querySelector('.thinking-line'), '::details-content').transitionDuration",
						),
						"0s",
						"生成 PNG 时关闭动效",
					);
					continue;
				}
				const source = await browser.evaluate<ExportDocumentSource>(`(${captureExportDocument.toString()})()`);
				const html = await pack.render(snapshot, source, AbortSignal.timeout(10_000));
				const file = join(root, `${theme}.html`);
				await writeFile(file, html);
				await browser.call("Network.enable");
				await browser.call("Network.emulateNetworkConditions", {
					offline: true,
					latency: 0,
					downloadThroughput: 0,
					uploadThroughput: 0,
				});
				await browser.call("Page.navigate", { url: pathToFileURL(file).href });
				await browser.until("location.protocol==='file:' && document.readyState==='complete'", "离线文档就绪");
				assert.equal(await browser.evaluate("document.querySelectorAll('details[open],script,button').length"), 0);
				const offlineHeights = await browser.evaluate<number[]>(`(async () => {
					const node = document.querySelector('.turn-process');
					const heights = [node.getBoundingClientRect().height];
					node.querySelector('summary').click();
					const start = performance.now();
					while (performance.now() - start < 350) { await new Promise(resolve => requestAnimationFrame(resolve)); heights.push(node.getBoundingClientRect().height); }
					return heights;
				})()`);
				assert.ok(
					offlineHeights.some(
						(height) => height > Math.min(...offlineHeights) + 1 && height < Math.max(...offlineHeights) - 1,
					),
					"离线 HTML 保留连续展开动效",
				);
				await browser.click(".thinking-line > summary");
				await browser.click(".tool-group > summary");
				await browser.click(".tool-group .tool-card summary");
				await browser.evaluate("document.fonts.ready");
				await browser.until(
					"document.querySelector('.thinking-line pre').checkVisibility() && document.querySelector('.tool-invocation').checkVisibility()",
					"离线展开动画呈现内容",
				);
				assert.equal(
					await browser.evaluate(
						"document.querySelector('.thinking-line pre').checkVisibility() && document.querySelector('.tool-invocation').checkVisibility()",
					),
					true,
				);
				assert.equal(await browser.evaluate("[...document.images].every(i=>i.naturalWidth>0)"), true);
				assert.equal(
					await browser.evaluate(
						"document.fonts.check('400 13px \"JetBrains Mono\"', 'Thinking fixture') && [...document.fonts].some(f=>f.family==='JetBrains Mono' && f.status==='loaded')",
					),
					true,
					"隐藏正文所用字体也已内嵌",
				);
				await browser.call("Emulation.setDeviceMetricsOverride", {
					width: 390,
					height: 844,
					deviceScaleFactor: 1,
					mobile: false,
				});
				assert.equal(await browser.evaluate("document.documentElement.scrollWidth<=innerWidth"), true);
				await browser.evaluate(
					"Promise.all(document.querySelector('.export-document').getAnimations({subtree:true}).map(animation=>animation.finished.catch(()=>{})))",
				);
				await browser.screenshot(join(root, `offline-${theme}-390.png`));
				await browser.call("Emulation.clearDeviceMetricsOverride");
				await browser.call("Network.emulateNetworkConditions", {
					offline: false,
					latency: 0,
					downloadThroughput: -1,
					uploadThroughput: -1,
				});
			}
		}
		console.info(`导出呈现证据：${root}`);
	} finally {
		pack.clear();
		await browser?.close();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	}
});
