import { useEffect, useState } from "react";
import type { ExportSnapshot } from "../../../../../agent/home/extensions/web/shared/exports";
import { ExportDocument } from "../exports/export-document";

/** 预览与 Puppeteer 共用的只读页面；快照经同源消息传入，不读取会话或账号接口。 */
export default function ExportRender() {
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
			document.querySelector(".export-document")?.getBoundingClientRect();
			await document.fonts.ready;
			await Promise.all([...document.images].map((image) => image.decode()));
			if ([...document.fonts].some((font) => font.status === "error")) throw new Error("导出字体无法加载");
			if (active) document.documentElement.dataset.exportState = "ready";
		})().catch(() => {
			if (active) {
				setError("导出内容或字体未能加载，请重新生成");
				document.documentElement.dataset.exportState = "failed";
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
		<ExportDocument snapshot={snapshot} />
	) : (
		<p>正在准备预览…</p>
	);
}
