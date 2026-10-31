import { Button } from "@heroui/react";
import { ArrowLeft, Download } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Link, useParams } from "react-router";
import type {
	ExportOptions,
	ExportSnapshot,
	SessionExport,
} from "../../../../../agent/home/extensions/web/shared/exports";
import { apiJson, sessionPath } from "../data/api";
import { ExportSelect } from "../exports/export-select";
import { ErrorNotice } from "../shell/error-notice";

function initialOptions(): ExportOptions {
	const theme = document.documentElement.dataset.theme;
	return {
		format: "png",
		theme:
			theme === "dark" || (theme === "system" && matchMedia("(prefers-color-scheme: dark)").matches)
				? "dark"
				: "light",
		includeThinking: false,
		includeTools: false,
	};
}

export default function ExportSession() {
	const { sessionId = "" } = useParams();
	const [options, setOptions] = useState<ExportOptions>(initialOptions);
	const [snapshot, setSnapshot] = useState<ExportSnapshot>();
	const [result, setResult] = useState<SessionExport>();
	const [busy, setBusy] = useState(false);
	const [error, setError] = useState("");
	const [previewError, setPreviewError] = useState("");
	const [scale, setScale] = useState(1);
	const frame = useRef<HTMLIFrameElement>(null);
	const preview = useRef<HTMLDivElement>(null);
	const latest = useRef(snapshot);
	latest.current = snapshot;
	const key = useRef<string | undefined>(undefined);
	const active = useRef(true);
	useEffect(() => {
		active.current = true;
		return () => {
			active.current = false;
		};
	}, []);
	useEffect(() => {
		const container = preview.current;
		if (!container) return;
		const observer = new ResizeObserver(() => setScale(Math.min(1, container.clientWidth / 1240)));
		observer.observe(container);
		return () => observer.disconnect();
	}, []);
	useEffect(() => {
		const receive = (event: MessageEvent) => {
			if (
				event.origin === location.origin &&
				event.source === frame.current?.contentWindow &&
				event.data?.type === "cst-export-frame-ready" &&
				latest.current
			)
				frame.current.contentWindow?.postMessage(
					{ type: "cst-export-preview", snapshot: latest.current },
					location.origin,
				);
		};
		window.addEventListener("message", receive);
		return () => window.removeEventListener("message", receive);
	}, []);
	useEffect(() => {
		key.current = undefined;
		setResult(undefined);
		setError("");
		setPreviewError("");
		setSnapshot(undefined);
		const controller = new AbortController();
		void apiJson<ExportSnapshot>(sessionPath(sessionId, "/exports/preview"), {
			method: "POST",
			body: options,
			signal: controller.signal,
		})
			.then((value) => {
				if (!controller.signal.aborted) setSnapshot(value);
			})
			.catch((cause) => {
				if (!controller.signal.aborted) setPreviewError(cause instanceof Error ? cause.message : "预览未能加载");
			});
		return () => controller.abort();
	}, [sessionId, options]);
	useEffect(() => {
		if (snapshot)
			frame.current?.contentWindow?.postMessage({ type: "cst-export-preview", snapshot }, location.origin);
	}, [snapshot]);
	useEffect(() => {
		if (!result || result.status === "completed" || result.status === "failed") return;
		const controller = new AbortController();
		const timer = window.setTimeout(() => {
			void apiJson<SessionExport>(`/api/exports/${result.id}`, { signal: controller.signal })
				.then(setResult)
				.catch((cause) => {
					if (!controller.signal.aborted) setError(cause instanceof Error ? cause.message : "无法读取导出进度");
				});
		}, 800);
		return () => {
			clearTimeout(timer);
			controller.abort();
		};
	}, [result]);
	const running = busy || result?.status === "queued" || result?.status === "rendering";
	async function generate() {
		if (running || !snapshot) return;
		setBusy(true);
		setError("");
		key.current ??= crypto.randomUUID();
		try {
			const value = await apiJson<SessionExport>(sessionPath(sessionId, "/exports"), {
				method: "POST",
				body: options,
				idempotencyKey: key.current,
			});
			if (active.current) {
				setResult(value);
				key.current = undefined;
			}
		} catch (cause) {
			if (active.current) setError(cause instanceof Error ? cause.message : "导出请求未完成");
		} finally {
			if (active.current) setBusy(false);
		}
	}
	return (
		<main className="export-workspace">
			<header className="export-toolbar">
				<Link to={`/s/${sessionId}`} className="export-back">
					<ArrowLeft size={18} />
					返回对话
				</Link>
				<h1>导出会话</h1>
			</header>
			<div className="export-layout">
				<section className="export-controls" aria-label="导出选项">
					<fieldset disabled={running}>
						<legend>导出选项</legend>
						<ExportSelect
							label="格式"
							value={options.format}
							items={[
								{ id: "png", label: "PNG 图片" },
								{ id: "html", label: "离线 HTML" },
							]}
							disabled={running}
							onChange={(format) => setOptions({ ...options, format })}
						/>
						<ExportSelect
							label="主题"
							value={options.theme}
							items={[
								{ id: "light", label: "日间" },
								{ id: "dark", label: "夜间" },
							]}
							disabled={running}
							onChange={(theme) => setOptions({ ...options, theme })}
						/>
						<label className="export-check">
							<input
								type="checkbox"
								checked={options.includeThinking}
								onChange={(event) => setOptions({ ...options, includeThinking: event.target.checked })}
							/>
							包含思考过程
						</label>
						<label className="export-check">
							<input
								type="checkbox"
								checked={options.includeTools}
								onChange={(event) => setOptions({ ...options, includeTools: event.target.checked })}
							/>
							包含工具结果
						</label>
					</fieldset>
					<Button className="export-generate" isDisabled={running || !snapshot} onPress={() => void generate()}>
						<Download size={16} />
						{running ? "正在生成…" : "生成并保存"}
					</Button>
					{error && (
						<ErrorNotice
							message={error}
							onRetry={() => {
								if (result && result.status !== "failed") {
									setError("");
									setResult({ ...result });
								} else void generate();
							}}
						/>
					)}
					{result?.status === "failed" && (
						<ErrorNotice message={result.error?.message ?? "导出未完成"} onRetry={() => void generate()} />
					)}
					{result?.status === "completed" && (
						<section className="export-result" aria-label="导出结果">
							<h2>已保存 {result.files.length} 个文件</h2>
							<p>{result.directory}</p>
							{result.files.map((file) => (
								<a key={file.url} href={file.url} download>
									{file.name}
								</a>
							))}
						</section>
					)}
					{running && <output className="export-note">任务在本机后台生成，离开页面后会继续保存。</output>}
				</section>
				<section className="export-preview-panel" aria-label="导出预览">
					<div className="export-preview-label">预览</div>
					{previewError && (
						<ErrorNotice message={previewError} onRetry={() => setOptions((value) => ({ ...value }))} />
					)}
					{!snapshot && !previewError && <output>正在准备预览…</output>}
					<div
						ref={preview}
						className="export-preview-viewport"
						style={{ height: 900 * scale, visibility: snapshot ? "visible" : "hidden" }}
					>
						<iframe
							ref={frame}
							src="/_export"
							title="会话导出预览"
							width="1240"
							height="900"
							style={{ transform: `scale(${scale})` }}
						/>
					</div>
				</section>
			</div>
		</main>
	);
}
