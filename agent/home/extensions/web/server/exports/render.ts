import type { ExportSnapshot } from "../../shared/exports.ts";
import { ExportBrowser } from "./browser.ts";
import { ExportError } from "./snapshot.ts";

export type ExportEmitter = (extension: "html" | "png", bytes: Buffer) => Promise<void>;
export type ExportRenderer = (
	snapshot: ExportSnapshot,
	url: string,
	emit: ExportEmitter,
	signal: AbortSignal,
) => Promise<void>;

/** API 与分享共用渲染器；单任务浏览器，逐页输出，不把整场截图堆在内存中。 */
export function createExportRenderer(workDir: string): ExportRenderer {
	return async (snapshot, url, emit, signal) => {
		signal.throwIfAborted();
		const browser = await ExportBrowser.launch(workDir);
		const stop = () => {
			void browser.close();
		};
		signal.addEventListener("abort", stop, { once: true });
		try {
			signal.throwIfAborted();
			await browser.ready(url);
			if (snapshot.options.format === "html") {
				// 内嵌字体会产生数 MiB 文本；分段读取，避免单条调试消息中断连接。
				const length = await browser.evaluate<number>(
					"typeof globalThis.cstExportHtml === 'string' ? globalThis.cstExportHtml.length : 0",
				);
				const chunks: string[] = [];
				for (let start = 0; start < length; start += 128 * 1024) {
					signal.throwIfAborted();
					chunks.push(
						await browser.evaluate<string>(`globalThis.cstExportHtml.slice(${start}, ${start + 128 * 1024})`),
					);
				}
				const html = chunks.join("");
				if (!html.startsWith("<!DOCTYPE html>")) throw new Error("离线文件未生成");
				await emit("html", Buffer.from(html, "utf8"));
				return;
			}
			const geometry = await browser.evaluate<{ height: number; cuts: number[] }>(`(() => {
				const height = Math.ceil(document.querySelector('.export-document').getBoundingClientRect().bottom + scrollY);
				const cuts = [...document.querySelectorAll('.export-message, .markdown > *, tr, .export-tool')]
					.map(node => Math.ceil(node.getBoundingClientRect().bottom + scrollY + 12));
				for (const node of document.querySelectorAll('pre')) {
					const rect = node.getBoundingClientRect(), style = getComputedStyle(node);
					const line = parseFloat(style.lineHeight);
					if (line > 0) for (let y = rect.top + scrollY + parseFloat(style.paddingTop) + line; y < rect.bottom + scrollY; y += line) cuts.push(Math.round(y));
				}
				return { height, cuts: cuts.sort((a,b)=>a-b) };
			})()`);
			if (geometry.height > 100_000)
				throw new ExportError(413, "export_too_tall", "图片超过导出高度限制，请缩短当前分支或改用 HTML。");
			await browser.call("Emulation.setDeviceMetricsOverride", {
				width: 1240,
				height: 3200,
				deviceScaleFactor: 2,
				mobile: false,
			});
			let top = 0;
			let cutIndex = 0;
			while (top < geometry.height) {
				signal.throwIfAborted();
				const target = Math.min(top + 3200, geometry.height);
				let cut = target;
				if (target < geometry.height) {
					while (cutIndex < geometry.cuts.length && geometry.cuts[cutIndex] <= target) {
						if (geometry.cuts[cutIndex] >= top + 1600) cut = geometry.cuts[cutIndex];
						cutIndex++;
					}
				}
				// 固定视口并滚动，只绘制当前分图，避免跨视口截图扩展并重绘长页面。
				await browser.evaluate(`scrollTo({ top: ${top}, behavior: 'instant' })`);
				const page = await browser.call<{ data: string }>("Page.captureScreenshot", {
					format: "png",
					optimizeForSpeed: true,
					captureBeyondViewport: false,
					clip: { x: 0, y: top, width: 1240, height: cut - top, scale: 1 },
				});
				await emit("png", Buffer.from(page.data, "base64"));
				top = cut;
			}
		} finally {
			signal.removeEventListener("abort", stop);
			await browser.close();
		}
	};
}
