import { ListChecks, Pencil, Trash2 } from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useLocation, useNavigate } from "react-router";
import { apiJson } from "../data/api";
import { SessionRenameDialog } from "./app-dialogs";
import { SessionDeleteDialog, SessionDeleteNotice, useSessionManagement } from "./session-management";
import { useWorkspaceSessions } from "./sessions-context";
import { usePresenceExit } from "./use-presence-exit";
import { useWorkspaceShell } from "./workspace-shell-context";

export type SessionItem = { id: string; title: string; updatedAt?: string };
export type SessionMenuState = { id: string; title: string; x: number; y: number; managing: boolean } | null;

/** 会话列表共用操作与删除恢复，列表外观由调用方决定。 */
export function useSessionActions(
	sessions: readonly SessionItem[],
	focusSession: (id?: string) => void,
	preview = false,
) {
	const state = useWorkspaceSessions();
	const shell = useWorkspaceShell();
	const location = useLocation();
	const navigate = useNavigate();
	const [menu, setMenu] = useState<SessionMenuState>(null);
	const [renaming, setRenaming] = useState<{ id: string; title: string } | null>(null);
	const menuRef = useRef<HTMLDivElement>(null);
	const clampedMenuPosRef = useRef<{ rawX: number; rawY: number; x: number; y: number } | null>(null);
	const deletionRoute = useRef<{ id: string; from: string; homeKey?: string } | null>(null);
	const management = useSessionManagement(sessions, {
		onStart: async (targets) => {
			state.beginDeletion(targets.map((session) => session.id));
			const current = targets.find(
				(session) => location.pathname === `/s/${session.id}` || location.pathname.startsWith(`/s/${session.id}/`),
			);
			if (!current) return;
			const route: { id: string; from: string; homeKey?: string } = {
				id: current.id,
				from: location.pathname + location.search,
			};
			deletionRoute.current = route;
			await navigate("/", { replace: true });
			if (window.location.pathname === "/") route.homeKey = window.history.state?.key;
		},
		onComplete: (report) => {
			state.settleDeletion(
				report.deleted.map((session) => session.id),
				report.failed.map((session) => session.id),
			);
			const route = deletionRoute.current;
			deletionRoute.current = null;
			if (
				route?.homeKey &&
				window.location.pathname === "/" &&
				window.history.state?.key === route.homeKey &&
				report.failed.some((session) => session.id === route.id)
			) {
				void navigate(route.from, { replace: true });
			}
			void state.refresh();
		},
	});

	const finishManagement = management.finish;
	const cancelDelete = management.cancel;
	// biome-ignore lint/correctness/useExhaustiveDependencies: 路由切换时结束未提交的列表操作
	useEffect(() => {
		setMenu(null);
		setRenaming(null);
		finishManagement();
	}, [location.pathname, finishManagement]);
	// 通道切入软件端待机态时，立刻撤下弹出菜单、重命名框与删除确认弹窗，避免原生 <dialog> 盖在待机遮罩之上。
	useEffect(() => {
		if (!shell?.appActive) return;
		setMenu(null);
		setRenaming(null);
		cancelDelete();
		finishManagement();
	}, [shell?.appActive, cancelDelete, finishManagement]);
	useLayoutEffect(() => {
		if (!menu) return;
		const element = menuRef.current;
		if (!element) return;
		// 用未受缩放影响的 offsetWidth/offsetHeight 夹紧位置，避免首次入场缩放低估尺寸导致贴边裁切。
		const width = element.offsetWidth || element.getBoundingClientRect().width;
		const height = element.offsetHeight || element.getBoundingClientRect().height;
		const marginX = Math.min(16, Math.max(8, (window.innerWidth - width) / 2));
		const marginY = Math.min(16, Math.max(8, (window.innerHeight - height) / 2));
		const x = Math.max(marginX, Math.min(menu.x, window.innerWidth - width - marginX));
		const y = Math.max(marginY, Math.min(menu.y, window.innerHeight - height - marginY));
		clampedMenuPosRef.current = { rawX: menu.x, rawY: menu.y, x, y };
		element.style.left = `${x}px`;
		element.style.top = `${y}px`;
		element.querySelector<HTMLElement>('[role="menuitem"]')?.focus();
	}, [menu]);
	useEffect(() => {
		if (!menu) return;
		const close = (event: Event) => {
			const target = event.target as Element | null;
			if (menuRef.current?.contains(target)) return;
			const trigger = target?.closest<HTMLElement>("[data-menu-trigger-id]");
			if (trigger?.dataset.menuTriggerId === menu.id) return;
			setMenu(null);
		};
		const dismiss = () => {
			const focusInside = Boolean(menuRef.current?.contains(document.activeElement));
			setMenu(null);
			if (focusInside) focusSession(menu.id);
		};
		window.addEventListener("pointerdown", close);
		window.addEventListener("scroll", dismiss, true);
		window.addEventListener("resize", dismiss);
		return () => {
			window.removeEventListener("pointerdown", close);
			window.removeEventListener("scroll", dismiss, true);
			window.removeEventListener("resize", dismiss);
		};
	}, [menu, focusSession]);
	useEffect(() => {
		if (!menu && !management.managing) return;
		const closeOnEscape = (event: KeyboardEvent) => {
			if (
				event.key !== "Escape" ||
				event.defaultPrevented ||
				event.isComposing ||
				event.keyCode === 229 ||
				management.targets
			)
				return;
			if (menu) {
				event.preventDefault();
				setMenu(null);
				focusSession(menu.id);
			} else if (!management.busy) {
				event.preventDefault();
				finishManagement();
				focusSession();
			}
		};
		window.addEventListener("keydown", closeOnEscape);
		return () => window.removeEventListener("keydown", closeOnEscape);
	}, [menu, management.managing, management.targets, management.busy, finishManagement, focusSession]);
	function openSessionMenu(session: { id: string; title: string }, x: number, y: number, toggle = false) {
		if (preview || management.busy || (management.managing && !management.count)) return;
		if (toggle && menu?.id === session.id) {
			setMenu(null);
			return;
		}
		setMenu({ ...session, x, y, managing: management.managing });
	}
	function rename(id: string, current: string) {
		setMenu(null);
		setRenaming({ id, title: current });
	}

	async function submitRename(id: string, nextTitle: string) {
		await apiJson(`/api/sessions/${encodeURIComponent(id)}`, { method: "PATCH", body: { name: nextTitle } });
		await state.refresh();
	}

	function cancelRename() {
		const targetId = renaming?.id;
		setRenaming(null);
		if (targetId) focusSession(targetId);
	}

	function clearRenaming() {
		setRenaming(null);
	}

	function remove(id: string) {
		setMenu(null);
		management.requestDelete(sessions.filter((session) => session.id === id));
	}

	return {
		menu,
		menuRef,
		clampedMenuPosRef,
		setMenu,
		renaming,
		cancelRename,
		clearRenaming,
		submitRename,
		management,
		openSessionMenu,
		rename,
		remove,
		focusSession,
	};
}

export function SessionActionsOverlay({ actions }: { actions: ReturnType<typeof useSessionActions> }) {
	const { menu, menuRef, clampedMenuPosRef, setMenu, management, rename, remove } = actions;
	const menuPresence = usePresenceExit(menu);
	const activeMenu = menuPresence.value;
	const clamped = clampedMenuPosRef.current;
	const menuLeft =
		activeMenu && clamped && clamped.rawX === activeMenu.x && clamped.rawY === activeMenu.y
			? clamped.x
			: activeMenu?.x;
	const menuTop =
		activeMenu && clamped && clamped.rawX === activeMenu.x && clamped.rawY === activeMenu.y
			? clamped.y
			: activeMenu?.y;
	return (
		<>
			{menuPresence.mounted &&
				activeMenu &&
				createPortal(
					<div
						className={`tier-2-glass tier-2-menu session-menu-surface${menuPresence.exiting ? "" : " session-menu"}`}
						ref={(node) => {
							menuRef.current = node;
							menuPresence.surfaceRef(node);
						}}
						role="menu"
						aria-label={activeMenu.managing ? "批量会话操作" : `会话 ${activeMenu.title}`}
						aria-hidden={menuPresence.exiting || undefined}
						data-state={menuPresence.state}
						data-exiting={menuPresence.exiting ? "true" : undefined}
						inert={menuPresence.exiting || undefined}
						style={{ left: menuLeft, top: menuTop }}
						onKeyDown={
							menuPresence.exiting
								? undefined
								: (event) => {
										if (event.nativeEvent.isComposing || event.keyCode === 229) return;
										if (event.key === "Tab" && !event.ctrlKey && !event.altKey && !event.metaKey) {
											const items = [
												...event.currentTarget.querySelectorAll<HTMLElement>('[role="menuitem"]'),
											];
											if (!items.length) return;
											event.preventDefault();
											const current = items.indexOf(document.activeElement as HTMLElement);
											const delta = event.shiftKey ? -1 : 1;
											const index =
												current < 0
													? event.shiftKey
														? items.length - 1
														: 0
													: (current + delta + items.length) % items.length;
											items[index]?.focus();
											return;
										}
										if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
										event.preventDefault();
										const items = [...event.currentTarget.querySelectorAll<HTMLElement>('[role="menuitem"]')];
										const current = items.indexOf(document.activeElement as HTMLElement);
										const index =
											event.key === "Home"
												? 0
												: event.key === "End"
													? items.length - 1
													: (current + (event.key === "ArrowDown" ? 1 : -1) + items.length) % items.length;
										items[index]?.focus();
									}
						}
					>
						{activeMenu.managing ? (
							<button
								type="button"
								role="menuitem"
								className="menu-row danger"
								onClick={() => {
									if (menuPresence.exiting) return;
									setMenu(null);
									management.requestDelete();
								}}
							>
								<Trash2 size={16} aria-hidden="true" />
								<span>删除</span>
							</button>
						) : (
							<>
								<button
									type="button"
									role="menuitem"
									className="menu-row"
									onClick={() => {
										if (menuPresence.exiting) return;
										void rename(activeMenu.id, activeMenu.title);
									}}
								>
									<Pencil size={16} aria-hidden="true" />
									<span>重命名</span>
								</button>
								<button
									type="button"
									role="menuitem"
									className="menu-row"
									onClick={() => {
										if (menuPresence.exiting) return;
										setMenu(null);
										management.start(activeMenu.id);
										actions.focusSession(activeMenu.id);
									}}
								>
									<ListChecks size={16} aria-hidden="true" />
									<span>管理</span>
								</button>
								<hr className="menu-sep" />
								<button
									type="button"
									role="menuitem"
									className="menu-row danger"
									onClick={() => {
										if (menuPresence.exiting) return;
										remove(activeMenu.id);
									}}
								>
									<Trash2 size={16} aria-hidden="true" />
									<span>删除</span>
								</button>
							</>
						)}
					</div>,
					document.body,
				)}
			{actions.renaming && (
				<SessionRenameDialog
					initialTitle={actions.renaming.title}
					onCancel={actions.cancelRename}
					onSubmit={(nextTitle) => actions.submitRename(actions.renaming!.id, nextTitle)}
				/>
			)}
			{management.targets && (
				<SessionDeleteDialog
					count={management.targets.length}
					onCancel={management.cancel}
					onConfirm={() => void management.confirm()}
				/>
			)}
			{management.result && !management.busy && (
				<SessionDeleteNotice result={management.result} onClose={management.dismissResult} />
			)}
		</>
	);
}
