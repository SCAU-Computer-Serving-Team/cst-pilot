import { type FormEvent, type RefObject, useCallback, useEffect, useId, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useDialogFocusTrap } from "./dialog-focus";
import { ErrorNotice } from "./error-notice";

function tokenDurationMs(element: HTMLElement, token: string, fallback: number): number {
	const raw = getComputedStyle(element).getPropertyValue(token).trim();
	const parsed = Number.parseFloat(raw);
	return Number.isFinite(parsed) ? parsed * (raw.endsWith("ms") ? 1 : 1000) : fallback;
}

/** 执行模态弹窗 150ms (--duration-quick) 关闭过渡后再卸载；支持入场中途打断与减少动态即时关闭。 */
function useAnimatedDialogClose(dialogRef: RefObject<HTMLDialogElement | null>): (onDone: () => void) => void {
	const closingRef = useRef(false);
	const cleanupRef = useRef<(() => void) | null>(null);

	useEffect(
		() => () => {
			cleanupRef.current?.();
		},
		[],
	);

	return useCallback(
		(onDone: () => void) => {
			if (closingRef.current) return;
			closingRef.current = true;
			const modal = dialogRef.current;
			const motion = window.matchMedia("(prefers-reduced-motion: reduce)");
			if (!modal || !modal.open || motion.matches) {
				onDone();
				return;
			}
			const style = getComputedStyle(modal);
			const startOpacity = style.opacity || "1";
			const startTransform = style.transform === "none" ? "translateY(0px) scale(1)" : style.transform;
			const distance = style.getPropertyValue("--distance-base").trim() || "8px";
			const scale = style.getPropertyValue("--scale-large").trim() || "0.96";
			const easing = style.getPropertyValue("--ease-smooth-out").trim() || "cubic-bezier(0.22, 1, 0.36, 1)";
			const duration = tokenDurationMs(modal, "--duration-quick", 150);

			modal.dataset.closing = "true";
			const animation = modal.animate(
				[
					{ opacity: startOpacity, transform: startTransform },
					{ opacity: 0, transform: `translateY(${distance}) scale(${scale})` },
				],
				{ duration, easing, fill: "forwards" },
			);

			let finished = false;
			const finish = () => {
				if (finished) return;
				finished = true;
				cleanupRef.current = null;
				motion.removeEventListener("change", onReduceMotion);
				animation.cancel();
				onDone();
			};
			const onReduceMotion = () => {
				if (motion.matches) finish();
			};
			motion.addEventListener("change", onReduceMotion);
			cleanupRef.current = () => {
				motion.removeEventListener("change", onReduceMotion);
				animation.cancel();
			};
			void animation.finished.then(finish, finish);
		},
		[dialogRef],
	);
}

function restorePreviousFocus(element: HTMLDialogElement | null, previous: Element | null): void {
	const active = document.activeElement;
	const focusRemainedInDialog = !active || active === document.body || Boolean(element?.contains(active));
	element?.close();
	if (focusRemainedInDialog && previous instanceof HTMLElement && previous.isConnected) {
		previous.focus();
	}
}

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
	const requestClose = useAnimatedDialogClose(dialog);
	useDialogFocusTrap(dialog, cancel);
	useEffect(() => {
		const previous = document.activeElement;
		const element = dialog.current;
		element?.showModal();
		cancel.current?.focus();
		return () => restorePreviousFocus(element, previous);
	}, []);
	return createPortal(
		<dialog
			ref={dialog}
			className="exit-dialog confirm-dialog"
			aria-labelledby={titleId}
			aria-describedby={descId}
			onCancel={(event) => {
				event.preventDefault();
				if (!busy) requestClose(onCancel);
			}}
		>
			<h2 id={titleId}>{title}</h2>
			<p id={descId} className="exit-description">
				{description}
			</p>
			<div className="exit-actions">
				<button type="button" ref={cancel} disabled={busy} onClick={() => requestClose(onCancel)}>
					{cancelLabel}
				</button>
				<button
					type="button"
					className={danger ? "exit-confirm" : "dialog-primary"}
					disabled={busy}
					onClick={() => requestClose(onConfirm)}
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
	const requestClose = useAnimatedDialogClose(dialog);
	useDialogFocusTrap(dialog, input);
	useEffect(() => {
		const previous = document.activeElement;
		const element = dialog.current;
		element?.showModal();
		input.current?.focus();
		input.current?.select();
		return () => restorePreviousFocus(element, previous);
	}, []);
	async function submit(event: FormEvent<HTMLFormElement>) {
		event.preventDefault();
		if (busy) return;
		const trimmed = name.trim();
		if (!trimmed) return;
		if (trimmed === initialTitle) {
			requestClose(onCancel);
			return;
		}
		setBusy(true);
		setError("");
		try {
			await onSubmit(trimmed);
			requestClose(onCancel);
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
				if (!busy) requestClose(onCancel);
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
					<button type="button" disabled={busy} onClick={() => requestClose(onCancel)}>
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
