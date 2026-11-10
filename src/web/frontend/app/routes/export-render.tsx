import { useEffect, useState } from "react";
import { useSearchParams } from "react-router";
import type { ExportSnapshot } from "../data/types";
import { ExportDocument } from "../exports/export-document";

/** 预览与 Puppeteer 共用的只读页面；快照经同源消息传入，不读取会话或账号接口。 */
export default function ExportRender() {
	const [searchParams] = useSearchParams();
	const preview = searchParams.get("preview") === "1";
	const [snapshot, setSnapshot] = useState<ExportSnapshot>();
	const [error, setError] = useState("");
	useEffect(() => {
		const receive = (event: MessageEvent) => {
			if (
				event.origin !== location.origin ||
				event.source !== window.parent ||
				event.data?.type !== "cst-export-preview"
			)
				return;
			setError("");
			setSnapshot(event.data.snapshot as ExportSnapshot);
		};
		window.addEventListener("message", receive);
		document.documentElement.dataset.exportReceiver = "ready";
		parent.postMessage({ type: "cst-export-frame-ready" }, location.origin);
		return () => {
			window.removeEventListener("message", receive);
			delete document.documentElement.dataset.exportReceiver;
		};
	}, []);
	useEffect(() => {
		if (!snapshot) return;
		let active = true;
		document.documentElement.dataset.theme = snapshot.options.theme;
		document.documentElement.dataset.exportState = "loading";
		document.body.classList.add("export-page");
		void (async () => {
			await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
			// 先触发布局，让本次正文使用的字体进入加载集合。
			const source = document.querySelector(".export-document");
			source?.getBoundingClientRect();
			// HTML 的折叠内容也要带上字体，离线展开后沿用相同排版。
			if (source && snapshot.options.format === "html") {
				const fonts = new Map<string, Set<string>>();
				const walker = document.createTreeWalker(source, NodeFilter.SHOW_TEXT);
				while (walker.nextNode()) {
					const node = walker.currentNode;
					if (!node.parentElement || !node.textContent?.trim()) continue;
					const style = getComputedStyle(node.parentElement);
					const font = `${style.fontStyle} ${style.fontWeight} ${style.fontSize} ${style.fontFamily}`;
					const characters = fonts.get(font) ?? new Set<string>();
					for (const character of node.textContent) characters.add(character);
					fonts.set(font, characters);
				}
				await Promise.all(
					[...fonts].map(([font, characters]) => document.fonts.load(font, [...characters].join(""))),
				);
			}
			await document.fonts.ready;
			await Promise.all([...document.images].map((image) => image.decode()));
			if ([...document.fonts].some((font) => font.status === "error")) throw new Error("导出字体无法加载");
			if (active) {
				document.documentElement.dataset.exportState = "ready";
				parent.postMessage({ type: "cst-export-content-ready" }, location.origin);
			}
		})().catch(() => {
			if (active) {
				setError("导出内容或字体未能加载，请重新生成");
				document.documentElement.dataset.exportState = "failed";
				parent.postMessage({ type: "cst-export-content-failed" }, location.origin);
			}
		});
		return () => {
			active = false;
			document.body.classList.remove("export-page");
		};
	}, [snapshot]);
	return error ? (
		<p role="alert">{error}</p>
	) : snapshot ? (
		<ExportDocument snapshot={snapshot} preview={preview} />
	) : (
		<p>正在准备预览…</p>
	);
}
