import { Ellipsis, ListChecks, MessagesSquare, Plus, Trash2 } from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { Link, useNavigate, useSearchParams } from "react-router";
import { KbdFocusOverlay, useKeyboardFocusStyle, usePointerMutex } from "../shell/kbd-focus";
import { isNavigationKeyEvent, scrollRowIntoView } from "../shell/keyboard-navigation";
import { sampleSessions } from "../shell/sample-sessions";
import { SessionActionsOverlay, useSessionActions } from "../shell/session-actions";
import { SessionEntry } from "../shell/session-entry";
import { groupSessions } from "../shell/session-list";
import { SessionManagementToolbar } from "../shell/session-management";
import { useWorkspaceSessions } from "../shell/sessions-context";
import { SidebarHeaderToggle, useWorkspaceShell } from "../shell/workspace-shell-context";

export default function Sessions() {
	const state = useWorkspaceSessions();
	const navigate = useNavigate();
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
	const contentRef = useRef<HTMLElement>(null);
	useLayoutEffect(() => {
		if (!preview) contentRef.current?.focus({ preventScroll: true });
	}, [preview]);
	const itemRefs = useRef(new Map<string, HTMLLIElement>());
	const kbdStyle = useKeyboardFocusStyle();
	const [kbdId, setKbdId] = useState<string | null>(null);
	const clearKeyboard = useCallback(() => setKbdId(null), []);
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
	const shell = useWorkspaceShell();
	const actions = useSessionActions(sessions, focusSession, preview);
	const { management, openSessionMenu, setMenu } = actions;
	const overlayBlocked =
		!!actions.menu || !!actions.renaming || !!management.targets || !!shell?.appActive || !!shell?.overlay;

	const kbdIndex = kbdId === null ? -1 : sessions.findIndex((session) => session.id === kbdId);
	const targetId = kbdIndex >= 0 && !overlayBlocked ? kbdId : null;

	useEffect(() => {
		if (kbdId !== null && kbdIndex < 0) clearKeyboard();
	}, [kbdId, kbdIndex, clearKeyboard]);

	usePointerMutex(targetId !== null, clearKeyboard);

	useLayoutEffect(() => {
		if (kbdIndex < 0 || targetId === null || !contentRef.current) return;
		const row = itemRefs.current.get(targetId);
		if (row) scrollRowIntoView(contentRef.current, row);
	}, [targetId, kbdIndex]);

	useEffect(() => {
		if (overlayBlocked || !sessions.length) return;
		const onKey = (event: KeyboardEvent) => {
			if (!isNavigationKeyEvent(event)) return;
			const target = event.target;
			const content = contentRef.current;
			if (!(target instanceof HTMLElement) || !content) return;
			const fromRow = content.contains(target) && target.matches(".sidebar-session, .session-native-check");
			const fromList = target === document.body || target === content;
			if (!fromList && !fromRow) return;
			const len = sessions.length;
			let next: number | undefined;
			switch (event.key) {
				case "ArrowDown":
					next = (kbdIndex + 1) % len;
					break;
				case "ArrowUp":
					next = kbdIndex < 0 ? len - 1 : (kbdIndex - 1 + len) % len;
					break;
				case "Home":
					next = 0;
					break;
				case "End":
					next = len - 1;
					break;
			}
			if (next !== undefined) {
				event.preventDefault();
				content.focus({ preventScroll: true });
				setKbdId(sessions[next].id);
				return;
			}
			if (!fromList || targetId === null || event.repeat || management.busy) return;
			if (event.key !== "Enter" && !(event.key === " " && management.managing)) return;
			event.preventDefault();
			if (management.managing) {
				setMenu(null);
				management.toggle(targetId);
			} else if (!preview) {
				navigate(`/s/${targetId}`, { viewTransition: true });
			}
		};
		window.addEventListener("keydown", onKey);
		return () => window.removeEventListener("keydown", onKey);
	}, [overlayBlocked, sessions, kbdIndex, targetId, management, setMenu, preview, navigate]);

	return (
		<main className="chat-main session-list-main" ref={root} data-managing={management.managing || undefined}>
			<header className="chat-header settings-header">
				<SidebarHeaderToggle />
				<MessagesSquare size={16} aria-hidden="true" />
				<h1>全部会话</h1>
				<button
					type="button"
					className="btn-ghost session-list-manage"
					disabled={!sessions.length || management.busy || preview}
					aria-pressed={management.managing}
					onClick={() => {
						if (management.managing) management.finish();
						else management.start();
					}}
				>
					<ListChecks size={16} aria-hidden="true" />
					管理
				</button>
			</header>
			<section
				className="session-list-content kbd-focus-scope"
				aria-label="会话列表"
				ref={contentRef}
				tabIndex={-1}
				data-kbd-active={targetId !== null || undefined}
				onBlur={(event) => {
					if (event.target === event.currentTarget && !overlayBlocked) clearKeyboard();
				}}
			>
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
							className="btn-ghost session-list-delete"
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
						<button type="button" className="tier-1-btn-outline" onClick={() => void state.refresh()}>
							重试
						</button>
					</div>
				)}
				{!sessions.length &&
					(!state.error || preview) &&
					(state.stage || preview ? (
						<div className="session-list-empty-state" aria-live="polite">
							<div className="session-list-empty-icon" aria-hidden="true">
								<MessagesSquare size={16} />
							</div>
							<h2>暂无历史会话</h2>
							<Link to="/" className="tier-1-btn-primary" viewTransition>
								<Plus size={16} aria-hidden="true" />
								新建对话
							</Link>
						</div>
					) : (
						<p className="session-list-empty" aria-live="polite">
							正在读取会话…
						</p>
					))}
				{groups.map((group) => (
					<section className="session-list-group" key={group.id} aria-labelledby={`session-group-${group.id}`}>
						<h2 id={`session-group-${group.id}`}>{group.label}</h2>
						<ul className="session-list" aria-label={group.label}>
							{group.sessions.map((session, index) => (
								<li
									key={session.id}
									className="session-list-item"
									data-menu-open={actions.menu?.id === session.id || undefined}
									ref={(el) => {
										if (el) itemRefs.current.set(session.id, el);
										else itemRefs.current.delete(session.id);
									}}
									onMouseDown={clearKeyboard}
								>
									<SessionEntry
										session={session}
										index={index}
										active={false}
										managing={management.managing}
										selected={management.selection?.has(session.id) ?? false}
										disabled={management.busy}
										preview={preview}
										menuOpen={actions.menu?.id === session.id}
										onToggle={() => {
											setMenu(null);
											management.toggle(session.id);
										}}
										onOpenMenu={(x, y) => openSessionMenu(session, x, y)}
									/>
									{!management.managing && !preview && (
										<button
											type="button"
											className="btn-icon sm-28 session-list-more"
											data-menu-trigger-id={session.id}
											aria-label={`会话操作：${session.title}`}
											aria-haspopup="menu"
											aria-expanded={actions.menu?.id === session.id}
											onClick={(event) => {
												const bounds = event.currentTarget.getBoundingClientRect();
												openSessionMenu(session, bounds.left, bounds.bottom + 4, true);
											}}
										>
											<Ellipsis size={16} aria-hidden="true" />
										</button>
									)}
								</li>
							))}
						</ul>
					</section>
				))}
				<KbdFocusOverlay
					containerRef={contentRef}
					itemRefs={itemRefs}
					targetId={targetId}
					style={kbdStyle}
					insetX={-3}
					insetY={-2}
				/>
			</section>
			<SessionActionsOverlay actions={actions} />
		</main>
	);
}
