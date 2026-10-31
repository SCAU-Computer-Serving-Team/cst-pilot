import { Ellipsis, ListChecks, MessagesSquare, Trash2 } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useSearchParams } from "react-router";
import { sampleSessions } from "../shell/sample-sessions";
import { SessionActionsOverlay, useSessionActions } from "../shell/session-actions";
import { SessionEntry } from "../shell/session-entry";
import { groupSessions } from "../shell/session-list";
import { SessionManagementToolbar } from "../shell/session-management";
import { useWorkspaceSessions } from "../shell/sessions-context";

export default function Sessions() {
	const state = useWorkspaceSessions();
	const [params] = useSearchParams();
	const preview = params.get("preview") === "1";
	const [today, setToday] = useState(() => new Date());
	useEffect(() => {
		const nextDay = new Date(today);
		nextDay.setHours(24, 0, 0, 0);
		const timer = setTimeout(() => setToday(new Date()), Math.max(0, nextDay.getTime() - Date.now()));
		return () => clearTimeout(timer);
	}, [today]);
	const groups = useMemo(
		() => groupSessions(preview ? sampleSessions : state.sessions, today),
		[preview, state.sessions, today],
	);
	const sessions = useMemo(() => groups.flatMap((group) => group.sessions), [groups]);
	const root = useRef<HTMLElement>(null);
	const focusSession = useCallback((id?: string) => {
		requestAnimationFrame(() => {
			const row = id ? root.current?.querySelector<HTMLElement>(`[data-session-id="${CSS.escape(id)}"]`) : undefined;
			(
				row?.querySelector<HTMLElement>("input") ??
				row ??
				root.current?.querySelector<HTMLElement>(".session-list-manage")
			)?.focus();
		});
	}, []);
	const actions = useSessionActions(sessions, focusSession, preview);
	const { management, openSessionMenu, setMenu } = actions;
	return (
		<main className="chat-main session-list-main" ref={root} data-managing={management.managing || undefined}>
			<header className="chat-header">
				<MessagesSquare size={22} aria-hidden="true" />
				<h1>全部会话</h1>
				<button
					type="button"
					className="session-list-manage"
					disabled={!sessions.length || management.busy || preview}
					aria-pressed={management.managing}
					onClick={() => {
						if (management.managing) management.finish();
						else management.start();
					}}
				>
					<ListChecks size={18} aria-hidden="true" />
					管理
				</button>
			</header>
			<div className="session-list-content">
				{management.managing && (
					<div className="session-list-toolbar">
						<span aria-live="polite">已选择 {management.count} 个</span>
						<SessionManagementToolbar
							active
							allSelected={management.allSelected}
							busy={management.busy}
							onToggleAll={management.toggleAll}
							onFinish={() => {
								management.finish();
								focusSession();
							}}
						/>
						<button
							type="button"
							className="session-list-delete"
							disabled={!management.count || management.busy}
							onClick={() => management.requestDelete()}
						>
							<Trash2 size={16} aria-hidden="true" />
							删除
						</button>
					</div>
				)}
				{state.error && !preview && (
					<div className="session-list-error" role="alert">
						<p>{state.error}</p>
						<button type="button" onClick={() => void state.refresh()}>
							重试
						</button>
					</div>
				)}
				{!sessions.length && (!state.error || preview) && (
					<p className="session-list-empty" aria-live="polite">
						{state.stage || preview ? "暂无会话" : "正在读取会话…"}
					</p>
				)}
				{groups.map((group) => (
					<section className="session-list-group" key={group.id} aria-labelledby={`session-group-${group.id}`}>
						<h2 id={`session-group-${group.id}`}>{group.label}</h2>
						<ul className="session-list" aria-label={group.label}>
							{group.sessions.map((session, index) => (
								<li key={session.id} className="session-list-item">
									<SessionEntry
										session={session}
										index={index}
										active={false}
										managing={management.managing}
										selected={management.selection?.has(session.id) ?? false}
										disabled={management.busy}
										preview={preview}
										onToggle={() => {
											setMenu(null);
											management.toggle(session.id);
										}}
										onContextMenu={(event) => {
											event.preventDefault();
											openSessionMenu(session, event.clientX, event.clientY);
										}}
									/>
									{!management.managing && !preview && (
										<button
											type="button"
											className="session-list-more"
											aria-label={`会话操作：${session.title}`}
											aria-haspopup="menu"
											aria-expanded={actions.menu?.id === session.id}
											onClick={(event) => {
												const bounds = event.currentTarget.getBoundingClientRect();
												openSessionMenu(session, bounds.left, bounds.bottom);
											}}
										>
											<Ellipsis size={18} aria-hidden="true" />
										</button>
									)}
								</li>
							))}
						</ul>
					</section>
				))}
			</div>
			<SessionActionsOverlay actions={actions} />
		</main>
	);
}
