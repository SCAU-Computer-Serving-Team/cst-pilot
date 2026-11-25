import { ArrowLeft, Download } from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { apiJson, sessionPath } from "../data/api";
import type { ExportOptions, ExportSnapshot, SessionExport } from "../data/types";
import { ExportPreview } from "../exports/export-preview";
import { ExportResult } from "../exports/export-result";
import { ExportSelect } from "../exports/export-select";
import { ErrorNotice } from "../shell/error-notice";
import { KbdFocusOverlay, useKeyboardFocusStyle, usePointerMutex } from "../shell/kbd-focus";
import { isNavigationKeyEvent } from "../shell/keyboard-navigation";
import { SidebarHeaderToggle, useWorkspaceShell } from "../shell/workspace-shell-context";

const EXPORT_FOCUS_KEYS = ["format", "theme", "thinking", "tools", "save", "open"] as const;
type ExportFocusKey = (typeof EXPORT_FOCUS_KEYS)[number];

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
	return <ExportSessionContent key={sessionId} sessionId={sessionId} />;
}

function ExportSessionContent({ sessionId }: { sessionId: string }) {
	const navigate = useNavigate();
	const shell = useWorkspaceShell();
	const [options, setOptions] = useState<ExportOptions>(initialOptions);
	const [snapshot, setSnapshot] = useState<ExportSnapshot>();
	const [result, setResult] = useState<SessionExport>();
	const [busy, setBusy] = useState(false);
	const [error, setError] = useState("");
	const [previewError, setPreviewError] = useState("");
	const [previewPending, setPreviewPending] = useState(true);
	const [openSelect, setOpenSelect] = useState<"format" | "theme" | null>(null);
	const [kbdIdx, setKbdIdx] = useState<number | null>(null);
	const key = useRef<string | undefined>(undefined);
	const active = useRef(true);
	const workspaceRef = useRef<HTMLElement>(null);
	const controlsRef = useRef<HTMLElement>(null);
	const itemRefs = useRef(new Map<string, HTMLElement>());
	const kbdStyle = useKeyboardFocusStyle();
	const clearKeyboard = useCallback(() => setKbdIdx(null), []);
	const setItemRef = useCallback(
		(id: ExportFocusKey) => (node: HTMLElement | null) => {
			if (node) itemRefs.current.set(id, node);
			else itemRefs.current.delete(id);
		},
		[],
	);
	useLayoutEffect(() => {
		controlsRef.current?.focus({ preventScroll: true });
	}, []);
	useEffect(() => {
		active.current = true;
		return () => {
			active.current = false;
		};
	}, []);
	useEffect(() => {
		key.current = undefined;
		setResult(undefined);
		setError("");
		setPreviewError("");
		setPreviewPending(true);
		const controller = new AbortController();
		void apiJson<ExportSnapshot>(sessionPath(sessionId, "/exports/preview"), {
			method: "POST",
			body: options,
			signal: controller.signal,
		})
			.then((value) => {
				if (!controller.signal.aborted) {
					setSnapshot(value);
					setPreviewPending(false);
				}
			})
			.catch((cause) => {
				if (!controller.signal.aborted) setPreviewError(cause instanceof Error ? cause.message : "预览未能加载");
			});
		return () => controller.abort();
	}, [sessionId, options]);
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
	const hasOpenButton = result?.status === "completed";
	const maxIdx = hasOpenButton ? 5 : 4;
	const overlayBlocked = !!shell?.appActive || !!shell?.overlay || openSelect !== null;
	const activeKbdKey: ExportFocusKey | null =
		!overlayBlocked && kbdIdx !== null ? EXPORT_FOCUS_KEYS[Math.min(kbdIdx, maxIdx)] : null;
	useEffect(() => {
		if (kbdIdx !== null && kbdIdx > maxIdx) setKbdIdx(maxIdx);
	}, [kbdIdx, maxIdx]);
	usePointerMutex(activeKbdKey !== null, clearKeyboard);
	const generate = useCallback(async () => {
		if (running || !snapshot || previewPending) return;
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
	}, [running, snapshot, previewPending, sessionId, options]);
	useEffect(() => {
		if (overlayBlocked) return;
		const onKey = (event: KeyboardEvent) => {
			if (!isNavigationKeyEvent(event)) return;
			const target = event.target;
			const workspace = workspaceRef.current;
			const controls = controlsRef.current;
			if (!(target instanceof HTMLElement) || !workspace || !controls) return;
			const inWorkspace = target === document.body || workspace.contains(target);
			if (!inWorkspace) return;
			if (event.key === "Escape") {
				event.preventDefault();
				navigate(`/s/${sessionId}`, { viewTransition: true });
				return;
			}
			const isArrowOrJump = ["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key);
			if (isArrowOrJump) {
				event.preventDefault();
				controls.focus({ preventScroll: true });
				const cur = kbdIdx === null ? 0 : Math.min(kbdIdx, maxIdx);
				if (event.key === "ArrowDown") {
					setKbdIdx(kbdIdx === null ? 0 : Math.min(maxIdx, cur + 1));
				} else if (event.key === "ArrowUp") {
					setKbdIdx(kbdIdx === null ? 0 : Math.max(0, cur - 1));
				} else if (event.key === "Home") {
					setKbdIdx(0);
				} else if (event.key === "End") {
					setKbdIdx(maxIdx);
				} else if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
					setKbdIdx(cur);
					if (!running) {
						if (cur === 0) {
							const nextFmt = event.key === "ArrowLeft" ? "png" : "html";
							setOptions((prev) => (prev.format === nextFmt ? prev : { ...prev, format: nextFmt }));
						} else if (cur === 1) {
							const nextTheme = event.key === "ArrowLeft" ? "light" : "dark";
							setOptions((prev) => (prev.theme === nextTheme ? prev : { ...prev, theme: nextTheme }));
						}
					}
				}
				return;
			}
			if (kbdIdx === null || event.repeat) return;
			if (event.key !== "Enter" && event.key !== " ") return;
			if (target !== controls && target !== document.body && target.closest("a, button, input")) return;
			event.preventDefault();
			const cur = Math.min(kbdIdx, maxIdx);
			if (cur === 0 && !running) setOpenSelect("format");
			else if (cur === 1 && !running) setOpenSelect("theme");
			else if (cur === 2 && !running) setOptions((prev) => ({ ...prev, includeThinking: !prev.includeThinking }));
			else if (cur === 3 && !running) setOptions((prev) => ({ ...prev, includeTools: !prev.includeTools }));
			else if (cur === 4) void generate();
			else if (cur === 5 && hasOpenButton) itemRefs.current.get("open")?.click();
		};
		window.addEventListener("keydown", onKey);
		return () => window.removeEventListener("keydown", onKey);
	}, [overlayBlocked, sessionId, navigate, kbdIdx, maxIdx, running, hasOpenButton, generate]);
	return (
		<main className="export-workspace" ref={workspaceRef}>
			<header className="export-toolbar">
				<SidebarHeaderToggle />
				<Link to={`/s/${sessionId}`} className="btn-ghost export-back" viewTransition>
					<ArrowLeft size={16} aria-hidden="true" />
					返回对话
				</Link>
				<h1>导出会话</h1>
			</header>
			<div className="export-layout">
				<section
					className="export-controls kbd-focus-scope"
					aria-label="导出选项"
					ref={controlsRef}
					tabIndex={-1}
					data-kbd-active={activeKbdKey !== null || undefined}
					onBlur={(event) => {
						if (event.target === event.currentTarget && !overlayBlocked) clearKeyboard();
					}}
				>
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
							open={openSelect === "format"}
							onOpenChange={(next) =>
								setOpenSelect(next ? "format" : openSelect === "format" ? null : openSelect)
							}
							triggerRef={setItemRef("format")}
							kbdHint={activeKbdKey === "format"}
							onChange={(format) => {
								clearKeyboard();
								setOptions({ ...options, format });
							}}
						/>
						<ExportSelect
							label="主题"
							value={options.theme}
							items={[
								{ id: "light", label: "日间" },
								{ id: "dark", label: "夜间" },
							]}
							disabled={running}
							open={openSelect === "theme"}
							onOpenChange={(next) => setOpenSelect(next ? "theme" : openSelect === "theme" ? null : openSelect)}
							triggerRef={setItemRef("theme")}
							kbdHint={activeKbdKey === "theme"}
							onChange={(theme) => {
								clearKeyboard();
								setOptions({ ...options, theme });
							}}
						/>
						<div className="export-checks">
							<label className="export-check" ref={setItemRef("thinking")}>
								<input
									type="checkbox"
									checked={options.includeThinking}
									onChange={(event) => {
										clearKeyboard();
										setOptions({ ...options, includeThinking: event.target.checked });
									}}
								/>
								包含思考过程
							</label>
							<label className="export-check" ref={setItemRef("tools")}>
								<input
									type="checkbox"
									checked={options.includeTools}
									onChange={(event) => {
										clearKeyboard();
										setOptions({ ...options, includeTools: event.target.checked });
									}}
								/>
								包含工具调用结果
							</label>
						</div>
					</fieldset>
					<button
						ref={setItemRef("save")}
						type="button"
						className="tier-1-btn-primary export-generate"
						disabled={running || !snapshot || previewPending}
						onClick={() => {
							clearKeyboard();
							void generate();
						}}
					>
						<Download size={16} aria-hidden="true" />
						{running ? "正在生成…" : "生成并保存"}
					</button>
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
						<ExportResult key={result.id} result={result} openButtonRef={setItemRef("open")} />
					)}
					{running && <output className="export-note">任务在本机后台生成，离开页面后会继续保存。</output>}
					<KbdFocusOverlay
						containerRef={controlsRef}
						itemRefs={itemRefs}
						targetId={activeKbdKey}
						style={kbdStyle}
						tone={activeKbdKey === "save" ? "accent" : "neutral"}
						railPlacement={activeKbdKey === "thinking" || activeKbdKey === "tools" ? "inside" : "outside"}
						bracketOutset={3}
					/>
				</section>
				<section className="export-preview-panel" aria-label="导出预览">
					<div className="export-preview-label">预览</div>
					{previewError && (
						<ErrorNotice message={previewError} onRetry={() => setOptions((value) => ({ ...value }))} />
					)}
					{!snapshot && !previewError && <output>正在准备预览…</output>}
					<ExportPreview
						snapshot={snapshot}
						pending={previewPending}
						onError={() => setPreviewError("导出内容或字体未能加载，请重试")}
					/>
				</section>
			</div>
		</main>
	);
}
