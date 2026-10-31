import { useEffect, useRef, useState } from "react";
import type { SessionExport } from "../../../../../agent/home/extensions/web/shared/exports";
import { apiJson } from "../data/api";
import { ErrorNotice } from "../shell/error-notice";

/** 结果仅展示数量与保存目录；打开失败单独重试，不重新生成产物。 */
export function ExportResult({ result }: { result: SessionExport }) {
	const [opening, setOpening] = useState(false);
	const [error, setError] = useState("");
	const request = useRef<AbortController | null>(null);
	const trigger = useRef<HTMLButtonElement>(null);
	useEffect(() => () => request.current?.abort(), []);
	async function openDirectory() {
		if (request.current) return;
		const controller = new AbortController();
		request.current = controller;
		setOpening(true);
		setError("");
		try {
			await apiJson<{ launched: boolean }>(`/api/exports/${result.id}/open`, {
				method: "POST",
				signal: controller.signal,
			});
		} catch (cause) {
			if (!controller.signal.aborted) setError(cause instanceof Error ? cause.message : "保存目录未能打开，请重试");
		} finally {
			if (request.current === controller) request.current = null;
			if (!controller.signal.aborted) {
				setOpening(false);
				requestAnimationFrame(() => {
					if (!controller.signal.aborted && document.activeElement === document.body) trigger.current?.focus();
				});
			}
		}
	}
	return (
		<section className="export-result" aria-label="导出结果">
			<h2>已保存 {result.files.length} 个文件</h2>
			<button
				ref={trigger}
				type="button"
				className="export-open-directory"
				aria-label="在文件管理器中打开保存目录"
				title="在文件管理器中打开保存目录"
				aria-busy={opening || undefined}
				disabled={opening}
				onClick={() => void openDirectory()}
			>
				{result.directory}
			</button>
			{error && <ErrorNotice message={error} onRetry={() => void openDirectory()} />}
		</section>
	);
}
