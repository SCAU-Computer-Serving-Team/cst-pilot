import { type FormEvent, useEffect, useId, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useDialogFocusTrap } from "./dialog-focus";
import { ErrorNotice } from "./error-notice";

export function ConfirmDialog({
	title,
	description,
	cancelLabel = "取消",
	confirmLabel,
	danger = false,
	busy = false,
	onCancel,
	onConfirm,
}: {
	title: string;
	description: string;
	cancelLabel?: string;
	confirmLabel: string;
	danger?: boolean;
	busy?: boolean;
	onCancel: () => void;
	onConfirm: () => void;
}) {
	const dialog = useRef<HTMLDialogElement>(null);
	const cancel = useRef<HTMLButtonElement>(null);
	const titleId = useId();
	const descId = useId();
	useDialogFocusTrap(dialog, cancel);
	useEffect(() => {
		const previous = document.activeElement;
		const element = dialog.current;
		element?.showModal();
		cancel.current?.focus();
		return () => {
			element?.close();
			if (previous instanceof HTMLElement && previous.isConnected) previous.focus();
		};
	}, []);
	return createPortal(
		<dialog
			ref={dialog}
			className="exit-dialog confirm-dialog"
			aria-labelledby={titleId}
			aria-describedby={descId}
			onCancel={(event) => {
				event.preventDefault();
				if (!busy) onCancel();
			}}
		>
			<h2 id={titleId}>{title}</h2>
			<p id={descId} className="exit-description">
				{description}
			</p>
			<div className="exit-actions">
				<button type="button" ref={cancel} disabled={busy} onClick={onCancel}>
					{cancelLabel}
				</button>
				<button
					type="button"
					className={danger ? "exit-confirm" : "dialog-primary"}
					disabled={busy}
					onClick={onConfirm}
				>
					{confirmLabel}
				</button>
			</div>
		</dialog>,
		document.body,
	);
}

export function SessionRenameDialog({
	initialTitle,
	onCancel,
	onSubmit,
}: {
	initialTitle: string;
	onCancel: () => void;
	onSubmit: (nextTitle: string) => Promise<void>;
}) {
	const dialog = useRef<HTMLDialogElement>(null);
	const input = useRef<HTMLInputElement>(null);
	const titleId = useId();
	const [name, setName] = useState(initialTitle);
	const [busy, setBusy] = useState(false);
	const [error, setError] = useState("");
	useDialogFocusTrap(dialog, input);
	useEffect(() => {
		const previous = document.activeElement;
		const element = dialog.current;
		element?.showModal();
		input.current?.focus();
		input.current?.select();
		return () => {
			element?.close();
			if (previous instanceof HTMLElement && previous.isConnected) previous.focus();
		};
	}, []);
	async function submit(event: FormEvent<HTMLFormElement>) {
		event.preventDefault();
		if (busy) return;
		const trimmed = name.trim();
		if (!trimmed) return;
		if (trimmed === initialTitle) {
			onCancel();
			return;
		}
		setBusy(true);
		setError("");
		try {
			await onSubmit(trimmed);
		} catch (cause) {
			setError(cause instanceof Error ? cause.message : "重命名失败");
			setBusy(false);
			requestAnimationFrame(() => input.current?.focus());
		}
	}
	return createPortal(
		<dialog
			ref={dialog}
			className="exit-dialog session-rename-dialog"
			aria-labelledby={titleId}
			onCancel={(event) => {
				event.preventDefault();
				if (!busy) onCancel();
			}}
		>
			<h2 id={titleId}>重命名会话</h2>
			<form className="dialog-form" onSubmit={(event) => void submit(event)}>
				<input
					ref={input}
					type="text"
					className="dialog-text-input"
					aria-label="会话名称"
					value={name}
					disabled={busy}
					onChange={(event) => setName(event.target.value)}
					onKeyDown={(event) => {
						if (event.key === "Enter" && (event.nativeEvent.isComposing || event.keyCode === 229)) {
							event.preventDefault();
						}
					}}
				/>
				{error && <ErrorNotice message={error} />}
				<div className="exit-actions">
					<button type="button" disabled={busy} onClick={onCancel}>
						取消
					</button>
					<button type="submit" className="dialog-primary" disabled={busy || !name.trim()}>
						{busy ? "正在保存…" : "保存"}
					</button>
				</div>
			</form>
		</dialog>,
		document.body,
	);
}
