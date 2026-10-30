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
				const html = await browser.evaluate<string>("globalThis.cstExportHtml");
				if (typeof html !== "string" || !html.startsWith("<!DOCTYPE html>")) throw new Error("离线文件未生成");
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
				throw new ExportError(413, "export_too_tall", "图片超过导出高度限制，请减少轮次或改用 HTML。");
			let top = 0;
			while (top < geometry.height) {
				signal.throwIfAborted();
				const target = Math.min(top + 3200, geometry.height);
				const cut =
					target === geometry.height
						? target
						: (geometry.cuts.filter((value) => value >= top + 1600 && value <= target).at(-1) ?? target);
				const page = await browser.call<{ data: string }>("Page.captureScreenshot", {
					format: "png",
					captureBeyondViewport: true,
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
