import type { ScreenshotOptions } from "puppeteer-core";
import type { ExportSnapshot } from "../../shared/exports.ts";
import { ExportBrowser } from "./browser.ts";
import { captureExportDocument, measureExportDocument } from "./document.ts";
import { OfflineHtml } from "./html.ts";
import { exportPages } from "./pagination.ts";
import { ExportError } from "./snapshot.ts";

export type ExportEmitter = (extension: "html" | "png", bytes: Buffer) => Promise<void>;
export type ExportRenderer = {
	render(snapshot: ExportSnapshot, emit: ExportEmitter, signal: AbortSignal): Promise<void>;
	close(): Promise<void>;
};

/** 快照直接注入专用页面；浏览器只承担排版，文件打包与持久化由后端负责。 */
export function createExportRenderer(options: {
	origin: string;
	workDir: string;
	executablePath?: string;
	idleMs?: number;
}): ExportRenderer {
	const origin = new URL(options.origin).origin;
	const browser = new ExportBrowser(options);
	const html = new OfflineHtml(origin);
	let fastEncoding = true;
	return {
		async render(snapshot, emit, signal) {
			await browser.run(signal, async (page) => {
				await page.setRequestInterception(true);
				page.on("request", (request) => {
					const url = new URL(request.url());
					const allowed =
						url.protocol === "data:" ||
						(request.method() === "GET" &&
							url.origin === origin &&
							(url.pathname === "/_export" || /^\/(assets|fonts)\//.test(url.pathname)));
					void (allowed ? request.continue() : request.abort("blockedbyclient")).catch(() => undefined);
				});
				await page.goto(`${origin}/_export`, { waitUntil: "domcontentloaded" });
				await page.waitForFunction(() => document.documentElement.dataset.exportReceiver === "ready", { signal });
				await page.evaluate((value) => {
					document.documentElement.dataset.exportState = "loading";
					window.postMessage({ type: "cst-export-preview", snapshot: value }, location.origin);
				}, snapshot);
				await page.waitForFunction(
					() => ["ready", "failed"].includes(document.documentElement.dataset.exportState ?? ""),
					{ signal },
				);
				if ((await page.evaluate(() => document.documentElement.dataset.exportState)) !== "ready")
					throw new ExportError(500, "export_document_failed", "导出正文、字体或附图无法加载，请重新生成");
				if (snapshot.options.format === "html") {
					const source = await page.evaluate(captureExportDocument);
					await emit("html", await html.render(snapshot, source, signal));
					return;
				}
				const geometry = await page.evaluate(measureExportDocument);
				for (const part of exportPages(geometry.height, geometry.cuts)) {
					signal.throwIfAborted();
					await page.evaluate((top) => scrollTo({ top, behavior: "instant" }), part.top);
					const screenshot: ScreenshotOptions = {
						type: "png",
						captureBeyondViewport: false,
						clip: { x: 0, y: part.top, width: 1240, height: part.height },
						...(fastEncoding ? { optimizeForSpeed: true } : {}),
					};
					let bytes: Uint8Array;
					try {
						bytes = await page.screenshot(screenshot);
					} catch (cause) {
						if (
							!fastEncoding ||
							!(cause instanceof Error) ||
							!/Invalid parameters|Unknown parameter|optimizeForSpeed/i.test(cause.message)
						)
							throw cause;
						fastEncoding = false;
						delete screenshot.optimizeForSpeed;
						bytes = await page.screenshot(screenshot);
					}
					await emit("png", Buffer.from(bytes));
				}
			});
		},
		async close() {
			await browser.close();
			html.clear();
		},
	};
}
