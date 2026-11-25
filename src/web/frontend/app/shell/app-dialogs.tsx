import { TriangleAlert } from "lucide-react";
import { type FormEvent, type RefObject, useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useDialogFocusTrap } from "./dialog-focus";
import { ErrorNotice } from "./error-notice";
import { handleTextFieldVerticalArrow } from "./input-shortcuts";
import { type DialogActionItem, KbdFocusOverlay, useDialogActionNav } from "./kbd-focus";
import { readDurationMs, readEasing } from "./motion-tokens";
import { useWorkspaceShell } from "./workspace-shell-context";

const RENAME_DIALOG_ACTIONS: readonly DialogActionItem[] = [
	{ id: "cancel", tone: "neutral" },
	{ id: "submit", tone: "accent" },
];

/** 软件端通道接管（appActive）时立即同步关闭原生顶层 <dialog> 并卸载，避免盖在待机遮罩之上。 */
export function useStandbyDialogClose(dialogRef: RefObject<HTMLDialogElement | null>, onDismiss: () => void): void {
	const shell = useWorkspaceShell();
	const dismissRef = useRef(onDismiss);
	dismissRef.current = onDismiss;
	useEffect(() => {
		if (!shell?.appActive) return;
		if (dialogRef.current?.open) dialogRef.current.close();
		dismissRef.current();
	}, [shell?.appActive, dialogRef]);
}

/** 执行模态弹窗 150ms (--duration-quick) 关闭过渡后再卸载；支持入场中途打断与减少动态即时关闭。 */
export function useAnimatedDialogClose(dialogRef: RefObject<HTMLDialogElement | null>) {
	const [closing, setClosing] = useState(false);
	const mounted = useRef(false);
	const closingRef = useRef(false);
	const cleanupRef = useRef<(() => void) | null>(null);

	useEffect(() => {
		mounted.current = true;
		return () => {
			mounted.current = false;
			cleanupRef.current?.();
			cleanupRef.current = null;
		};
	}, []);

	const requestClose = useCallback(
		(onDone: () => void) => {
			if (!mounted.current || closingRef.current) return;
			closingRef.current = true;
			setClosing(true);
			const modal = dialogRef.current;
			const motion = window.matchMedia("(prefers-reduced-motion: reduce)");
			if (!modal?.open || motion.matches) {
				if (modal?.open) modal.close();
				onDone();
				return;
			}
			const style = getComputedStyle(modal);
			const startOpacity = style.opacity || "1";
			const startTransform = style.transform === "none" ? "scale(1)" : style.transform;
			const scale = style.getPropertyValue("--scale-modal").trim() || "0.96";
			const easing = readEasing(modal);
			const duration = readDurationMs(modal, "--duration-quick", 150);

			modal.dataset.closing = "true";
			const animation = modal.animate(
				[
					{ opacity: startOpacity, transform: startTransform },
					{ opacity: 0, transform: `scale(${scale})` },
				],
				{ duration, easing, fill: "forwards" },
			);

			let finished = false;
			const finish = () => {
				if (finished || !mounted.current) return;
				finished = true;
				cleanupRef.current = null;
				motion.removeEventListener("change", onReduceMotion);
				animation.cancel();
				if (modal.open) modal.close();
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
	return { requestClose, closing };
}

export function restorePreviousFocus(element: HTMLDialogElement | null, previous: Element | null): void {
	const active = document.activeElement;
	const focusRemainedInDialog = !active || active === document.body || Boolean(element?.contains(active));
	if (element?.open) element.close();
	if (
		focusRemainedInDialog &&
		!document.querySelector(".standby-scrim") &&
		previous instanceof HTMLElement &&
		previous !== document.body &&
		previous.isConnected &&
		!previous.closest("[inert]")
	) {
		previous.focus({ preventScroll: true });
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
	const { requestClose, closing } = useAnimatedDialogClose(dialog);
	const actions = useMemo<readonly DialogActionItem[]>(
		() => [
			{ id: "cancel", tone: "neutral" },
			{ id: "confirm", tone: danger ? "danger" : "accent" },
		],
		[danger],
	);
	const actionNav = useDialogActionNav(actions, { initialId: "cancel", disabled: busy || closing });
	useDialogFocusTrap(dialog, cancel);
	useStandbyDialogClose(dialog, onCancel);
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
			className="tier-3-dialog exit-dialog confirm-dialog"
			aria-labelledby={titleId}
			aria-describedby={descId}
			onKeyDown={actionNav.onDialogKeyDown}
			onCancel={(event) => {
				event.preventDefault();
				if (!busy && !closing) requestClose(onCancel);
			}}
		>
			<div className={`dialog-title-row ${danger ? "dialog-title-row--danger" : "dialog-title-row--info"}`}>
				{danger && <TriangleAlert size={16} aria-hidden="true" />}
				<h2 id={titleId}>{title}</h2>
			</div>
			<p id={descId} className="exit-description">
				{description}
			</p>
			<div
				ref={actionNav.containerRef}
				className="exit-actions kbd-focus-scope kbd-dialog-actions"
				data-kbd-active={actionNav.kbdActive ? "true" : "false"}
			>
				<button
					type="button"
					ref={actionNav.registerButton("cancel", cancel)}
					className="tier-1-btn-outline"
					disabled={busy || closing}
					onPointerDown={actionNav.onButtonPointerDown}
					onFocus={() => actionNav.onButtonFocus("cancel")}
					onClick={() => requestClose(onCancel)}
				>
					{cancelLabel}
				</button>
				<button
					type="button"
					ref={actionNav.registerButton("confirm")}
					className={danger ? "tier-1-btn-danger solid-danger exit-confirm" : "tier-1-btn-primary dialog-primary"}
					disabled={busy || closing}
					onPointerDown={actionNav.onButtonPointerDown}
					onFocus={() => actionNav.onButtonFocus("confirm")}
					onClick={() => requestClose(onConfirm)}
				>
					{confirmLabel}
				</button>
				<KbdFocusOverlay
					containerRef={actionNav.containerRef}
					itemRefs={actionNav.itemRefs}
					targetId={actionNav.targetId}
					style={actionNav.kbdStyle}
					tone={actionNav.activeTone}
					railPlacement="bottom"
					bracketOutset={3}
				/>
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
	const { requestClose, closing } = useAnimatedDialogClose(dialog);
	const actionNav = useDialogActionNav(RENAME_DIALOG_ACTIONS, {
		requireButtonFocus: true,
		disabled: busy || closing,
	});
	useDialogFocusTrap(dialog, input);
	useStandbyDialogClose(dialog, onCancel);
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
		if (busy || closing || !dialog.current?.open) return;
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
			className="tier-3-dialog exit-dialog session-rename-dialog"
			aria-labelledby={titleId}
			onKeyDown={actionNav.onDialogKeyDown}
			onCancel={(event) => {
				event.preventDefault();
				if (!busy && !closing) requestClose(onCancel);
			}}
		>
			<div className="dialog-title-row">
				<h2 id={titleId}>重命名会话</h2>
			</div>
			<form className="dialog-form" onSubmit={(event) => void submit(event)}>
				<input
					ref={input}
					type="text"
					className="dialog-text-input"
					aria-label="会话名称"
					value={name}
					disabled={busy || closing}
					onChange={(event) => setName(event.target.value)}
					onKeyDown={(event) => {
						if (event.key === "Enter" && (event.nativeEvent.isComposing || event.keyCode === 229)) {
							event.preventDefault();
							return;
						}
						handleTextFieldVerticalArrow(event);
					}}
				/>
				{error && <ErrorNotice message={error} />}
				<div
					ref={actionNav.containerRef}
					className="exit-actions kbd-focus-scope kbd-dialog-actions"
					data-kbd-active={actionNav.kbdActive ? "true" : "false"}
				>
					<button
						type="button"
						ref={actionNav.registerButton("cancel")}
						className="tier-1-btn-outline"
						disabled={busy || closing}
						onPointerDown={actionNav.onButtonPointerDown}
						onFocus={() => actionNav.onButtonFocus("cancel")}
						onClick={() => requestClose(onCancel)}
					>
						取消
					</button>
					<button
						type="submit"
						ref={actionNav.registerButton("submit")}
						className="tier-1-btn-primary dialog-primary"
						disabled={busy || closing || !name.trim()}
						onPointerDown={actionNav.onButtonPointerDown}
						onFocus={() => actionNav.onButtonFocus("submit")}
					>
						{busy ? "正在保存…" : "保存"}
					</button>
					<KbdFocusOverlay
						containerRef={actionNav.containerRef}
						itemRefs={actionNav.itemRefs}
						targetId={actionNav.targetId}
						style={actionNav.kbdStyle}
						tone={actionNav.activeTone}
						railPlacement="bottom"
						bracketOutset={3}
					/>
				</div>
			</form>
		</dialog>,
		document.body,
	);
}
