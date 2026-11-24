import { TriangleAlert } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { apiJson } from "../data/api";
import { useAnimatedDialogClose, useStandbyDialogClose } from "./app-dialogs";
import { useDialogFocusTrap } from "./dialog-focus";
import { ErrorNotice } from "./error-notice";

type Lifecycle = { stopping: boolean; sessions: { id: string; title?: string; running: boolean }[] };
export function ExitDialog({ onClose }: { onClose: () => void }) {
	const dialog = useRef<HTMLDialogElement>(null);
	const cancel = useRef<HTMLButtonElement>(null);
	const [state, setState] = useState<Lifecycle>();
	const [error, setError] = useState("");
	const [busy, setBusy] = useState(false);
	const [exited, setExited] = useState(false);
	const { requestClose, closing } = useAnimatedDialogClose(dialog);
	useDialogFocusTrap(dialog, cancel);
	useStandbyDialogClose(dialog, onClose);
	useEffect(() => {
		const element = dialog.current;
		element?.showModal();
		cancel.current?.focus();
		let active = true;
		apiJson<Lifecycle>("/api/lifecycle")
			.then((value) => {
				if (active) setState(value);
			})
			.catch((cause) => {
				if (active) setError(String(cause));
			});
		return () => {
			active = false;
			if (element?.open) element.close();
			if (document.querySelector(".standby-scrim")) return;
			const target = [
				...document.querySelectorAll<HTMLElement>(
					".sidebar-account-trigger, .sidebar-restore-inline, .sidebar-restore",
				),
			].find((candidate) => !candidate.closest("[inert]"));
			target?.focus({ preventScroll: true });
		};
	}, []);
	useEffect(() => {
		if (exited) dialog.current?.focus({ preventScroll: true });
	}, [exited]);
	async function confirm() {
		if (busy || closing || !state) return;
		setBusy(true);
		setError("");
		try {
			await apiJson("/api/lifecycle/exit", { method: "POST", body: { confirm: "stop" } });
			setExited(true);
		} catch (cause) {
			setError(cause instanceof Error ? cause.message : "退出未完成");
			setBusy(false);
		}
	}
	const count = state?.sessions.filter((s) => s.running).length ?? 0;
	return createPortal(
		<dialog
			className="tier-3-dialog exit-dialog"
			data-exited={exited || undefined}
			ref={dialog}
			tabIndex={-1}
			aria-labelledby="exit-title"
			onCancel={(event) => {
				event.preventDefault();
				if (!busy && !closing) requestClose(onClose);
			}}
		>
			<div className="dialog-title-row dialog-title-row--danger">
				<TriangleAlert size={16} aria-hidden="true" />
				<h2 id="exit-title">{exited ? "CST Pilot 已退出" : "退出 CST Pilot？"}</h2>
			</div>
			{exited ? (
				<p className="exit-description">会话已保存。重开请运行 pi.cmd。</p>
			) : (
				<>
					{(!error || state) && (
						<p>
							{!state
								? "正在检查任务…"
								: count
									? `${count} 个会话运行中；退出将停止任务、暂停队列。`
									: "将关闭服务和终端。"}
						</p>
					)}
					{count > 0 && (
						<ul className="exit-session-list">
							{state?.sessions
								.filter((s) => s.running)
								.map((s) => (
									<li key={s.id}>{s.title ?? "会话"}</li>
								))}
						</ul>
					)}
					{state && <p className="exit-description">{count ? "记录保留，任务不自动重放。" : "会话保留。"}</p>}
					{error && <ErrorNotice message={error} />}
					<div className="exit-actions">
						<button
							type="button"
							ref={cancel}
							className="tier-1-btn-outline"
							disabled={busy || closing}
							onClick={() => requestClose(onClose)}
						>
							继续使用
						</button>
						<button
							type="button"
							className="tier-1-btn-danger solid-danger exit-confirm"
							disabled={busy || closing || !state}
							onClick={() => void confirm()}
						>
							{busy ? "正在退出…" : count ? "停止任务并退出" : "退出程序"}
						</button>
					</div>
				</>
			)}
		</dialog>,
		document.body,
	);
}
