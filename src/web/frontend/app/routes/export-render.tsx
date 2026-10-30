import { useEffect, useState } from "react";
import type { ExportSnapshot } from "../../../../../agent/home/extensions/web/shared/exports";
import { ExportDocument } from "../exports/export-document";
import { offlineHtml } from "../exports/offline-html";

/** 导出专用文档：浏览器渲染与页面预览共用，独立主题，不订阅会话或加载账号。 */
export default function ExportRender() {
	const [snapshot, setSnapshot] = useState<ExportSnapshot>();
	const [error, setError] = useState("");
	useEffect(() => {
		const controller = new AbortController();
		const hash = location.hash.slice(1);
		if (hash) {
			const [id, token] = hash.split(":");
			void fetch(`/api/exports/${encodeURIComponent(id)}/snapshot`, {
				headers: { "X-CST-Export-Token": token ?? "" },
				signal: controller.signal,
			})
				.then(async (response) => {
					if (!response.ok) throw new Error("导出预览已失效");
					setSnapshot((await response.json()) as ExportSnapshot);
				})
				.catch(() => {
					if (!controller.signal.aborted) {
						setError("无法读取导出快照");
						document.documentElement.dataset.exportState = "failed";
					}
				});
		}
		const receive = (event: MessageEvent) => {
			if (
				hash ||
				event.origin !== location.origin ||
				event.source !== window.parent ||
				event.data?.type !== "cst-export-preview"
			)
				return;
			setSnapshot(event.data.snapshot as ExportSnapshot);
		};
		window.addEventListener("message", receive);
		if (!hash) parent.postMessage({ type: "cst-export-frame-ready" }, location.origin);
		return () => {
			controller.abort();
			window.removeEventListener("message", receive);
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
			await document.fonts.ready;
			await Promise.all([...document.images].map((image) => image.decode()));
			if (!active) return;
			if (location.hash && snapshot.options.format === "html")
				Reflect.set(globalThis, "cstExportHtml", await offlineHtml(snapshot.title, snapshot.options.theme));
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
