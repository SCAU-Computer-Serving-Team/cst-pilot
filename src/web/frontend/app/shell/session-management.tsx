import { CircleAlert, CircleCheck, TriangleAlert, X } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useAnimatedDialogClose, useStandbyDialogClose } from "./app-dialogs";
import { useDialogFocusTrap } from "./dialog-focus";
import { type DialogActionItem, KbdFocusOverlay, useDialogActionNav } from "./kbd-focus";
import { Notification } from "./notification";
import {
	type DeletionResult,
	type DeletionTarget,
	deleteSessions,
	deletionReason,
	deletionTitle,
} from "./session-deletion";

const DELETE_DIALOG_ACTIONS: readonly DialogActionItem[] = [
	{ id: "cancel", tone: "neutral" },
	{ id: "confirm", tone: "danger" },
];

export function useSessionManagement(
	sessions: readonly DeletionTarget[],
	events: {
		onStart: (targets: readonly DeletionTarget[]) => void | Promise<void>;
		onComplete: (result: DeletionResult) => void;
	},
) {
	const [selection, setSelection] = useState<Set<string> | null>(null);
	const [targets, setTargets] = useState<DeletionTarget[] | null>(null);
	const [result, setResult] = useState<DeletionResult | null>(null);
	const [busy, setBusy] = useState(false);
	const working = useRef(false);
	const actions = useRef(events);
	actions.current = events;
	const selected = sessions.filter((session) => selection?.has(session.id));
	const count = busy && targets ? targets.length : selected.length;
	const finish = useCallback(() => {
		if (working.current) return;
		setSelection(null);
		setTargets(null);
	}, []);
	const cancel = useCallback(() => {
		if (!working.current) setTargets(null);
	}, []);
	function start(id?: string) {
		if (working.current) return;
		setResult(null);
		setSelection(new Set(id ? [id] : []));
	}
	function toggle(id: string) {
		if (working.current) return;
		setSelection((previous) => {
			if (!previous) return previous;
			const next = new Set(previous);
			if (next.has(id)) next.delete(id);
			else next.add(id);
			return next;
		});
	}
	function toggleAll() {
		if (working.current || !sessions.length) return;
		setSelection(selected.length === sessions.length ? new Set() : new Set(sessions.map((session) => session.id)));
	}
	const retain = useCallback((validIds: ReadonlySet<string>) => {
		if (working.current) return;
		setSelection((previous) => {
			if (!previous) return previous;
			let changed = false;
			const next = new Set<string>();
			for (const id of previous) {
				if (validIds.has(id)) next.add(id);
				else changed = true;
			}
			return changed ? next : previous;
		});
	}, []);
	function requestDelete(items: readonly DeletionTarget[] = selected) {
		if (!items.length || working.current) return;
		setTargets(items.map(({ id, title }) => ({ id, title })));
	}
	async function confirm() {
		if (!targets?.length || working.current) return;
		working.current = true;
		setBusy(true);
		setResult(null);
		const batch = targets;
		const navigation = Promise.resolve(actions.current.onStart(batch)).catch(() => undefined);
		setTargets(null);
		setSelection(null);
		try {
			const report = await deleteSessions(batch);
			await navigation;
			actions.current.onComplete(report);
			setResult(report);
		} finally {
			working.current = false;
			setBusy(false);
			setTargets(null);
			setSelection(null);
		}
	}
	return {
		managing: selection !== null,
		selection,
		count,
		allSelected: sessions.length > 0 && selected.length === sessions.length,
		targets,
		result,
		busy,
		start,
		finish,
		toggle,
		toggleAll,
		retain,
		requestDelete,
		confirm,
		cancel,
		dismissResult: () => setResult(null),
	};
}

export function SessionManagementToolbar({
	allSelected,
	hasItems = true,
	selectedCount,
	busy,
	active,
	onToggleAll,
	onDelete,
	onFinish,
}: {
	allSelected: boolean;
	hasItems?: boolean;
	selectedCount?: number;
	busy: boolean;
	active: boolean;
	onToggleAll: () => void;
	onDelete?: () => void;
	onFinish: () => void;
}) {
	return (
		<fieldset className="session-management-toolbar" aria-label="会话管理" inert={!active} aria-hidden={!active}>
			<button type="button" disabled={busy || !hasItems} onClick={onToggleAll}>
				{allSelected ? "取消全选" : "全选"}
			</button>
			{onDelete && (selectedCount ?? 0) > 0 && (
				<button type="button" className="session-management-delete" disabled={busy} onClick={onDelete}>
					删除
				</button>
			)}
			<button type="button" disabled={busy} onClick={onFinish}>
				完成
			</button>
		</fieldset>
	);
}

export function SessionDeleteDialog({
	count,
	onCancel,
	onConfirm,
}: {
	count: number;
	onCancel: () => void;
	onConfirm: () => void;
}) {
	const dialog = useRef<HTMLDialogElement>(null);
	const cancel = useRef<HTMLButtonElement>(null);
	const { requestClose, closing } = useAnimatedDialogClose(dialog);
	const actionNav = useDialogActionNav(DELETE_DIALOG_ACTIONS, { initialId: "cancel", disabled: closing });
	useDialogFocusTrap(dialog, cancel);
	useStandbyDialogClose(dialog, onCancel);
	useEffect(() => {
		const element = dialog.current;
		const previous = document.activeElement;
		element?.showModal();
		cancel.current?.focus();
		return () => {
			if (element?.open) element.close();
			if (document.querySelector(".standby-scrim")) return;
			if (previous instanceof HTMLElement && previous.isConnected && !previous.closest("[inert]")) {
				previous.focus({ preventScroll: true });
				return;
			}
			const fallback = [
				...document.querySelectorAll<HTMLElement>(
					".session-list-manage, .sidebar-session.selected, .sidebar-new, .sidebar-restore-inline, .sidebar-restore",
				),
			].find((candidate) => !candidate.closest("[inert]") && !candidate.hasAttribute("disabled"));
			fallback?.focus({ preventScroll: true });
		};
	}, []);
	return createPortal(
		<dialog
			ref={dialog}
			className="tier-3-dialog exit-dialog session-delete-dialog"
			aria-labelledby="session-delete-title"
			aria-describedby="session-delete-warning"
			onKeyDown={actionNav.onDialogKeyDown}
			onCancel={(event) => {
				event.preventDefault();
				if (!closing) requestClose(onCancel);
			}}
		>
			<div className="dialog-title-row dialog-title-row--danger">
				<TriangleAlert size={16} aria-hidden="true" />
				<h2 id="session-delete-title">删除 {count} 个会话？</h2>
			</div>
			<p id="session-delete-warning" className="exit-description">
				删除后无法恢复。
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
					disabled={closing}
					onPointerDown={actionNav.onButtonPointerDown}
					onFocus={() => actionNav.onButtonFocus("cancel")}
					onClick={() => requestClose(onCancel)}
				>
					取消
				</button>
				<button
					type="button"
					ref={actionNav.registerButton("confirm")}
					className="tier-1-btn-danger solid-danger exit-confirm"
					disabled={closing}
					onPointerDown={actionNav.onButtonPointerDown}
					onFocus={() => actionNav.onButtonFocus("confirm")}
					onClick={() => requestClose(onConfirm)}
				>
					删除
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

export function SessionDeleteNotice({ result, onClose }: { result: DeletionResult; onClose: () => void }) {
	const Icon = result.failed.length ? (result.deleted.length ? TriangleAlert : CircleAlert) : CircleCheck;
	const kind = result.failed.length ? (result.deleted.length ? "warning" : "danger") : "success";
	return (
		<Notification className="session-delete-notice" label="删除结果" kind={kind}>
			<Icon size={16} className="session-delete-status" aria-hidden="true" />
			<div className="session-delete-copy">
				<p>{deletionTitle(result)}</p>
				{result.failed.length === 1 ? (
					<p className="session-delete-reason">
						{result.failed[0].title}：{deletionReason(result.failed[0].error)}
					</p>
				) : result.failed.length > 1 ? (
					<details>
						<summary>查看原因</summary>
						<ul>
							{result.failed.map((item) => (
								<li key={item.id}>
									{item.title}：{deletionReason(item.error)}
								</li>
							))}
						</ul>
					</details>
				) : null}
			</div>
			<button
				type="button"
				className="btn-icon xs-24 notification-close session-delete-close"
				aria-label="关闭删除结果"
				onClick={onClose}
			>
				<X size={14} aria-hidden="true" />
			</button>
		</Notification>
	);
}
