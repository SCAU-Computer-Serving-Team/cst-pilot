import { ListChecks, Pencil, Trash2 } from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useLocation, useNavigate } from "react-router";
import { apiJson } from "../data/api";
import { SessionDeleteDialog, SessionDeleteNotice, useSessionManagement } from "./session-management";
import { useWorkspaceSessions } from "./sessions-context";

export type SessionItem = { id: string; title: string; updatedAt?: string };
export type SessionMenuState = { id: string; title: string; x: number; y: number } | null;

/** 会话列表共用操作与删除恢复，列表外观由调用方决定。 */
export function useSessionActions(
	sessions: readonly SessionItem[],
	focusSession: (id?: string) => void,
	preview = false,
) {
	const state = useWorkspaceSessions();
	const location = useLocation();
	const navigate = useNavigate();
	const [menu, setMenu] = useState<SessionMenuState>(null);
	const menuRef = useRef<HTMLDivElement>(null);
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
	// biome-ignore lint/correctness/useExhaustiveDependencies: 路由切换时结束未提交的列表操作
	useEffect(() => {
		setMenu(null);
		finishManagement();
	}, [location.pathname, finishManagement]);
	useLayoutEffect(() => {
		if (!menu) return;
		const element = menuRef.current;
		if (!element) return;
		const bounds = element.getBoundingClientRect();
		element.style.left = `${Math.max(8, Math.min(menu.x, window.innerWidth - bounds.width - 8))}px`;
		element.style.top = `${Math.max(8, Math.min(menu.y, window.innerHeight - bounds.height - 8))}px`;
		element.querySelector<HTMLElement>('[role="menuitem"]')?.focus();
	}, [menu]);
	useEffect(() => {
		if (!menu) return;
		const close = (event: Event) => {
			if (!menuRef.current?.contains(event.target as Node)) setMenu(null);
		};
		const dismiss = () => setMenu(null);
		window.addEventListener("pointerdown", close);
		window.addEventListener("scroll", dismiss, true);
		window.addEventListener("resize", dismiss);
		return () => {
			window.removeEventListener("pointerdown", close);
			window.removeEventListener("scroll", dismiss, true);
			window.removeEventListener("resize", dismiss);
		};
	}, [menu]);
	useEffect(() => {
		if (!menu && !management.managing) return;
		const closeOnEscape = (event: KeyboardEvent) => {
			if (event.key !== "Escape" || event.defaultPrevented || event.isComposing || management.targets) return;
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
	function openSessionMenu(session: { id: string; title: string }, x: number, y: number) {
		if (preview || management.busy || (management.managing && !management.count)) return;
		setMenu({ ...session, x, y });
	}
	async function rename(id: string, current: string) {
		setMenu(null);
		const name = window.prompt("会话名称", current);
		if (!name || !name.trim() || name.trim() === current) return;
		try {
			await apiJson(`/api/sessions/${encodeURIComponent(id)}`, { method: "PATCH", body: { name: name.trim() } });
			await state.refresh();
		} catch (cause) {
			window.alert(cause instanceof Error ? cause.message : "重命名失败");
		}
	}

	function remove(id: string) {
		setMenu(null);
		management.requestDelete(sessions.filter((session) => session.id === id));
	}

	return { menu, menuRef, setMenu, management, openSessionMenu, rename, remove, focusSession };
}

export function SessionActionsOverlay({ actions }: { actions: ReturnType<typeof useSessionActions> }) {
	const { menu, menuRef, setMenu, management, rename, remove } = actions;
	return (
		<>
			{menu &&
				createPortal(
					<div
						className="session-menu"
						ref={menuRef}
						role="menu"
						aria-label={management.managing ? "批量会话操作" : `会话 ${menu.title}`}
						style={{ left: menu.x, top: menu.y }}
						onKeyDown={(event) => {
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
						}}
					>
						{management.managing ? (
							<button
								type="button"
								role="menuitem"
								className="sidebar-account-item sidebar-menu-delete"
								onClick={() => {
									setMenu(null);
									management.requestDelete();
								}}
							>
								<Trash2 size={18} aria-hidden="true" />
								删除
							</button>
						) : (
							<>
								<button
									type="button"
									role="menuitem"
									className="sidebar-account-item"
									onClick={() => void rename(menu.id, menu.title)}
								>
									<Pencil size={18} aria-hidden="true" />
									重命名
								</button>
								<button
									type="button"
									role="menuitem"
									className="sidebar-account-item"
									onClick={() => {
										setMenu(null);
										management.start(menu.id);
										actions.focusSession(menu.id);
									}}
								>
									<ListChecks size={18} aria-hidden="true" />
									管理
								</button>
								<hr className="session-menu-divider" />
								<button
									type="button"
									role="menuitem"
									className="sidebar-account-item sidebar-menu-delete"
									onClick={() => remove(menu.id)}
								>
									<Trash2 size={18} aria-hidden="true" />
									删除
								</button>
							</>
						)}
					</div>,
					document.body,
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
