import { CircleAlert, CircleCheck, TriangleAlert, X } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Notification } from "./notification";
import {
	type DeletionResult,
	type DeletionTarget,
	deleteSessions,
	deletionReason,
	deletionTitle,
} from "./session-deletion";

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
		if (working.current) return;
		setSelection(selected.length === sessions.length ? new Set() : new Set(sessions.map((session) => session.id)));
	}
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
		requestDelete,
		confirm,
		cancel: () => {
			if (!working.current) setTargets(null);
		},
		dismissResult: () => setResult(null),
	};
}

export function SessionManagementToolbar({
	allSelected,
	busy,
	active,
	onToggleAll,
	onFinish,
}: {
	allSelected: boolean;
	busy: boolean;
	active: boolean;
	onToggleAll: () => void;
	onFinish: () => void;
}) {
	return (
		<fieldset className="session-management-toolbar" aria-label="会话管理" inert={!active} aria-hidden={!active}>
			<button type="button" disabled={busy} onClick={onToggleAll}>
				{allSelected ? "取消全选" : "全选"}
			</button>
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
	useEffect(() => {
		const element = dialog.current;
		const previous = document.activeElement;
		element?.showModal();
		cancel.current?.focus();
		return () => {
			element?.close();
			if (previous instanceof HTMLElement && previous.isConnected) previous.focus();
			else document.querySelector<HTMLElement>(".sidebar-session.selected, .sidebar-new")?.focus();
		};
	}, []);
	return createPortal(
		<dialog
			ref={dialog}
			className="exit-dialog session-delete-dialog"
			aria-labelledby="session-delete-title"
			aria-describedby="session-delete-warning"
			onCancel={(event) => {
				event.preventDefault();
				onCancel();
			}}
		>
			<h2 id="session-delete-title">删除 {count} 个会话？</h2>
			<p id="session-delete-warning" className="exit-description">
				删除后无法恢复。
			</p>
			<div className="exit-actions">
				<button type="button" ref={cancel} onClick={onCancel}>
					取消
				</button>
				<button type="button" className="exit-confirm" onClick={onConfirm}>
					删除
				</button>
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
			<Icon size={18} className="session-delete-status" aria-hidden="true" />
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
				className="notification-close session-delete-close"
				aria-label="关闭删除结果"
				onClick={onClose}
			>
				<X size={18} aria-hidden="true" />
			</button>
		</Notification>
	);
}
