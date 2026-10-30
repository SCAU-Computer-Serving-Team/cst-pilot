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
import type { SessionDetail } from "../data/web-state";
import { ErrorNotice } from "../shell/error-notice";

function initialOptions(): ExportOptions {
	const now = new Date();
	const theme = document.documentElement.dataset.theme;
	return {
		format: "png",
		theme:
			theme === "dark" || (theme === "system" && matchMedia("(prefers-color-scheme: dark)").matches)
				? "dark"
				: "light",
		date: `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}-${String(now.getDate()).padStart(2, "0")}`,
		includeThinking: false,
		includeTools: false,
	};
}

export default function ExportSession() {
	const { sessionId = "" } = useParams();
	const [options, setOptions] = useState<ExportOptions>(initialOptions);
	const [turns, setTurns] = useState<{ id: string; label: string }[]>([]);
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
	const usable = !options.turnIds || options.turnIds.length > 0;
	useEffect(() => {
		active.current = true;
		return () => {
			active.current = false;
		};
	}, []);
	useEffect(() => {
		const controller = new AbortController();
		void apiJson<SessionDetail>(sessionPath(sessionId), { signal: controller.signal })
			.then((detail) => {
				setTurns(
					detail.entries
						.filter((entry) => entry.message.role === "user")
						.map((entry) => ({
							id: entry.id,
							label:
								(typeof entry.message.content === "string"
									? entry.message.content
									: entry.message.content
											.filter((part) => part.type === "text")
											.map((part) => part.text)
											.join(" ")
								).slice(0, 100) || "图片消息",
						})),
				);
			})
			.catch((cause) => {
				if (!controller.signal.aborted) setPreviewError(cause instanceof Error ? cause.message : "轮次读取失败");
			});
		return () => controller.abort();
	}, [sessionId]);
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
		if (!usable) return;
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
	}, [sessionId, options, usable]);
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
		if (running || !snapshot || !usable) return;
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
						<label>
							格式
							<select
								value={options.format}
								onChange={(event) =>
									setOptions({ ...options, format: event.target.value as ExportOptions["format"] })
								}
							>
								<option value="png">PNG 图片</option>
								<option value="html">离线 HTML</option>
							</select>
						</label>
						<label>
							主题
							<select
								value={options.theme}
								onChange={(event) =>
									setOptions({ ...options, theme: event.target.value as ExportOptions["theme"] })
								}
							>
								<option value="light">日间</option>
								<option value="dark">夜间</option>
							</select>
						</label>
						<label>
							归档日期
							<input
								type="date"
								value={options.date}
								onChange={(event) => setOptions({ ...options, date: event.target.value })}
							/>
						</label>
						<label>
							范围
							<select
								value={options.turnIds ? "selected" : "all"}
								onChange={(event) =>
									setOptions({ ...options, turnIds: event.target.value === "all" ? undefined : [] })
								}
							>
								<option value="all">当前分支全部对话</option>
								<option value="selected">选择轮次</option>
							</select>
						</label>
						{options.turnIds && (
							<section className="export-turns" aria-label="选择轮次">
								{turns.map((turn, index) => (
									<label key={turn.id}>
										<input
											type="checkbox"
											checked={options.turnIds?.includes(turn.id)}
											onChange={(event) =>
												setOptions({
													...options,
													turnIds: event.target.checked
														? [...(options.turnIds ?? []), turn.id]
														: options.turnIds?.filter((id) => id !== turn.id),
												})
											}
										/>
										<span>
											{index + 1}. {turn.label}
										</span>
									</label>
								))}
							</section>
						)}
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
					<p className="export-note">
						保存到 artifacts/{options.date || "日期"}/sessions/。内容可能含机主信息，分享前请核对。
					</p>
					<Button
						className="export-generate"
						isDisabled={running || !snapshot || !usable}
						onPress={() => void generate()}
					>
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
					<output className="export-note">
						{running
							? "任务在本机后台生成，离开页面后会继续保存。"
							: "PNG 长会话自动分图；HTML 可离线阅读和折叠过程。"}
					</output>
				</section>
				<section className="export-preview-panel" aria-label="导出预览">
					<div className="export-preview-label">预览</div>
					{previewError && (
						<ErrorNotice message={previewError} onRetry={() => setOptions((value) => ({ ...value }))} />
					)}
					{!usable && <p>选择至少一轮对话。</p>}
					{!snapshot && usable && !previewError && <output>正在准备预览…</output>}
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
