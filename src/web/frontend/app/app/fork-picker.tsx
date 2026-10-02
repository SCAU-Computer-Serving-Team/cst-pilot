import { Button } from "@heroui/react";
import { X } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { apiJson } from "./api";
import type { ContentPart, Message } from "./web-state";

/** 与聊天页同规则：字符串原样，数组取全部文本段。 */
const textOf = (message: Message) =>
	typeof message.content === "string"
		? message.content
		: message.content
				.filter((part): part is Extract<ContentPart, { type: "text" }> => part.type === "text")
				.map((part) => part.text)
				.join("\n");

export type ForkTarget = { id: string; parentId?: string | null; text: string };

/**
 * 派生选择对话框（画布 n3Qkm/utKHY）：列出本会话全部用户消息，默认选中最后一条；
 * 确认后新会话在所选消息之前分支，消息文本预填新会话的输入框（对齐 TUI /fork）。
 */
export function ForkPicker({
	sessionId,
	targets,
	running,
	onClose,
}: {
	sessionId: string;
	targets: ForkTarget[];
	running: boolean;
	onClose: () => void;
}) {
	const navigate = useNavigate();
	const [cursor, setCursor] = useState(() => Math.max(0, targets.length - 1));
	const [busy, setBusy] = useState(false);
	const [error, setError] = useState("");
	const list = useRef<HTMLUListElement>(null);
	const [thumb, setThumb] = useState({ top: 0, height: 0, show: false });

	const selected = targets[cursor];
	const runnable = !running && !busy && !!selected && selected.parentId != null && targets.length > 0;

	const updateThumb = useCallback(() => {
		const el = list.current;
		if (!el) return;
		const { scrollHeight, clientHeight, scrollTop } = el;
		if (scrollHeight <= clientHeight) {
			setThumb((value) => (value.show ? { ...value, show: false } : value));
			return;
		}
		const height = Math.max((clientHeight / scrollHeight) * clientHeight, 28);
		const top = (scrollTop / (scrollHeight - clientHeight)) * (clientHeight - height);
		setThumb({ top, height, show: true });
	}, []);

	// 初始滚到最底端：默认选中最后一条，滚动条与选中行都要落位。
	useEffect(() => {
		const el = list.current;
		if (!el) return;
		el.scrollTop = el.scrollHeight;
		updateThumb();
	}, [updateThumb]);

	useEffect(() => {
		const el = list.current;
		if (!el) return;
		const row = el.children[cursor] as HTMLElement | undefined;
		row?.scrollIntoView({ block: "nearest" });
		updateThumb();
	}, [cursor, updateThumb]);

	const confirm = useCallback(
		async (target: ForkTarget) => {
			if (!runnable || target.parentId == null) return;
			setBusy(true);
			setError("");
			try {
				const created = await apiJson<{ id: string }>(`/api/sessions/${encodeURIComponent(sessionId)}/fork`, {
					method: "POST",
					body: { entryId: target.parentId },
				});
				sessionStorage.setItem(`cst-draft:${created.id}`, target.text);
				navigate(`/s/${created.id}`);
			} catch (cause) {
				setError(cause instanceof Error ? cause.message : "派生失败，请重试");
				setBusy(false);
			}
		},
		[runnable, navigate, sessionId],
	);

	useEffect(() => {
		const onKey = (event: KeyboardEvent) => {
			if (event.key === "Escape") {
				event.stopPropagation();
				onClose();
				return;
			}
			if (event.key === "ArrowUp" || event.key === "ArrowDown") {
				event.preventDefault();
				event.stopPropagation();
				setCursor((value) =>
					event.key === "ArrowUp" ? Math.max(0, value - 1) : Math.min(targets.length - 1, value + 1),
				);
				return;
			}
			if (event.key === "Enter") {
				event.preventDefault();
				event.stopPropagation();
				if (selected) void confirm(selected);
			}
		};
		window.addEventListener("keydown", onKey);
		return () => window.removeEventListener("keydown", onKey);
	}, [onClose, confirm, selected, targets.length]);

	// 弹层打开时把焦点从输入框移走，避免 Enter 同时触发输入框提交。
	useEffect(() => {
		list.current?.focus();
	}, []);

	return (
		<div className="summary-scrim" role="none" onMouseDown={onClose}>
			<div
				className="fork-dialog"
				role="dialog"
				aria-modal="true"
				aria-label="派生会话"
				onMouseDown={(event) => event.stopPropagation()}
			>
				<div className="summary-dialog-head">
					<h2>派生会话</h2>
					<Button variant="ghost" isIconOnly aria-label="关闭" onPress={onClose}>
						<X size={18} />
					</Button>
				</div>
				<div className="fork-list-wrap">
					<ul className="fork-list" ref={list} tabIndex={-1} aria-label="选择派生点">
						{targets.map((target, index) => (
							<li key={target.id}>
								<button
									type="button"
									className={`fork-row ${index === cursor ? "fork-row-active" : ""}`}
									disabled={busy || target.parentId == null}
									title={target.parentId == null ? "首条消息之前没有可分支的位置" : undefined}
									onMouseEnter={() => setCursor(index)}
									onClick={() => void confirm(target)}
								>
									{target.text}
								</button>
							</li>
						))}
					</ul>
					{thumb.show && (
						<span className="fork-thumb" style={{ top: thumb.top, height: thumb.height }} aria-hidden="true" />
					)}
				</div>
				{running && (
					<div className="connection-banner" role="alert">
						会话执行中，禁止派生
					</div>
				)}
				{!!error && (
					<div className="connection-banner" role="alert">
						{error}
					</div>
				)}
			</div>
		</div>
	);
}

/** 从会话条目提取派生候选：全部有文本的用户消息，分支点取其 parentId（消息之前）。 */
export function forkTargets(entries: { id: string; parentId?: string | null; message: Message }[]): ForkTarget[] {
	return entries
		.filter((entry) => entry.message.role === "user" && textOf(entry.message).trim())
		.map((entry) => ({ id: entry.id, parentId: entry.parentId, text: textOf(entry.message).trim() }));
}
