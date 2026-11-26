import { CheckCircle2, CircleAlert, Globe, PanelLeft, X } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Outlet, useLocation, useNavigate, useSearchParams, useViewTransitionState } from "react-router";
import { AccountProvider } from "../account/use-account";
import { apiJson, sessionPath } from "../data/api";
import { MessageOutboxRecovery } from "../data/use-message-outbox";
import type { SessionDetail } from "../data/web-state";
import { CommandPalette } from "../palette/command-palette";
import {
	extractActiveSessionId,
	type PaletteMode,
	type PaletteTarget,
	type RecentExportSummary,
	willPaletteTargetChangeRoute,
} from "../palette/palette-model";
import { BlueHour } from "../shell/blue-hour";
import { ConnectionStatus } from "../shell/connection-status";
import { focusableElements, resolveDialogTabTarget } from "../shell/dialog-focus";
import { Notification, NotificationRegion } from "../shell/notification";
import { SessionsProvider, useWorkspaceSessions } from "../shell/sessions-context";
import { Sidebar } from "../shell/shell";
import { useSidebarMotion } from "../shell/sidebar-motion";
import { usePresenceExit } from "../shell/use-presence-exit";
import { WorkspaceShellProvider } from "../shell/workspace-shell-context";

/** 侧栏折叠偏好：1 = 收起，0 = 展开；无记录时按窗口宽度定初值。 */
const SIDEBAR_KEY = "cst-ui-sidebar";
/** 半屏（≤960px）侧栏改为浮层盖在内容上，收起时不占布局位。 */
const NARROW_QUERY = "(max-width: 960px)";

function useMediaQuery(query: string) {
	const [matches, setMatches] = useState(() => window.matchMedia(query).matches);
	useEffect(() => {
		const list = window.matchMedia(query);
		setMatches(list.matches);
		const onChange = () => setMatches(list.matches);
		list.addEventListener("change", onChange);
		return () => list.removeEventListener("change", onChange);
	}, [query]);
	return matches;
}

/** 通道待机全屏遮罩 + 居中 384px 卡片（对齐 cst-pilot-web.pen `sYHwZ` / `WO6NQ`）。 */
function ChannelStandbyOverlay({ onRefresh }: { onRefresh: () => Promise<void> | void }) {
	const cardRef = useRef<HTMLDivElement>(null);
	const actionRef = useRef<HTMLButtonElement>(null);
	const [checking, setChecking] = useState(false);
	useEffect(() => {
		const previous = document.activeElement;
		actionRef.current?.focus({ preventScroll: true });
		// 原生顶层 <dialog> 在同一提交中关闭时可能竞争焦点，下一任务兜底确保待机主按钮持焦。
		const focusTimer = window.setTimeout(() => {
			if (cardRef.current && !cardRef.current.contains(document.activeElement)) {
				actionRef.current?.focus({ preventScroll: true });
			}
		}, 0);
		const onKeyDown = (event: KeyboardEvent) => {
			if (
				event.key !== "Tab" ||
				event.ctrlKey ||
				event.altKey ||
				event.metaKey ||
				event.isComposing ||
				event.keyCode === 229 ||
				!cardRef.current
			)
				return;
			const items = focusableElements(cardRef.current);
			const active =
				document.activeElement instanceof HTMLElement && cardRef.current.contains(document.activeElement)
					? document.activeElement
					: null;
			const target = resolveDialogTabTarget(items, active, event.shiftKey);
			if (!target) return;
			event.preventDefault();
			if (target === "dialog") cardRef.current.focus();
			else target.focus();
		};
		window.addEventListener("keydown", onKeyDown, true);
		return () => {
			window.clearTimeout(focusTimer);
			window.removeEventListener("keydown", onKeyDown, true);
			if (previous instanceof HTMLElement && previous !== document.body && previous.isConnected) {
				const restore = () => {
					if (previous.isConnected && !previous.closest("[inert]")) {
						previous.focus({ preventScroll: true });
					}
				};
				restore();
				if (document.activeElement !== previous) window.setTimeout(restore, 0);
			}
		};
	}, []);
	async function handleRefresh() {
		if (checking) return;
		setChecking(true);
		try {
			await onRefresh();
		} finally {
			setChecking(false);
		}
	}
	return (
		<div className="standby-scrim tier-3-scrim">
			<div
				ref={cardRef}
				className="standby-card tier-3-dialog"
				role="alertdialog"
				aria-modal="true"
				aria-labelledby="channel-standby-title"
				aria-describedby="channel-standby-desc"
				tabIndex={-1}
			>
				<div className="dialog-title-row dialog-title-row--info">
					<Globe size={16} aria-hidden="true" />
					<h2 id="channel-standby-title">软件端正在使用中</h2>
				</div>
				<p id="channel-standby-desc">Web 界面处于待机状态，草稿已保留。从软件端打开 Web 后可继续操作。</p>
				<div className="standby-actions">
					<button
						ref={actionRef}
						type="button"
						className="tier-1-btn-primary"
						disabled={checking}
						onClick={() => void handleRefresh()}
					>
						{checking ? "正在检查…" : "重新检查"}
					</button>
				</div>
			</div>
		</div>
	);
}

/** 工作台外壳：侧栏只挂载一次，路由切换时不再重建，避免会话列表闪空。 */
export default function WorkspaceLayout() {
	const { pathname } = useLocation();
	const [params] = useSearchParams();
	const preview = params.get("preview") === "1";
	const home = pathname === "/";
	const [startup, setStartup] = useState(true);
	// 离开首屏路径后不恢复启动标记，站内返回只使用页面转场。
	const startupPath = useRef<string | null>(pathname);
	if (startupPath.current !== pathname) startupPath.current = null;
	const initialPage = startupPath.current === pathname;
	useEffect(() => {
		if (!startup) return;
		if (window.location.hash === "#cst-web-start")
			window.history.replaceState(window.history.state, "", window.location.pathname + window.location.search);
		const timer = window.setTimeout(() => setStartup(false), 1200);
		return () => window.clearTimeout(timer);
	}, [startup]);
	const settings = pathname === "/settings" || pathname.startsWith("/settings/");
	const homeTransition = useViewTransitionState("/");
	const chatTransition = useViewTransitionState("/s/*");
	const accountTransition = useViewTransitionState("/account");
	const exportTransition = useViewTransitionState("/s/:sessionId/export");
	const settingsBoundary = settings && (homeTransition || chatTransition || accountTransition);
	const sessionTransition = chatTransition && !homeTransition && !settingsBoundary && !settings;
	const routeTransition = useViewTransitionState(pathname);
	// 同一路径固定一次入场策略，快照结束后不补播局部动效。
	const entry = useRef({ pathname, local: !routeTransition });
	if (entry.current.pathname !== pathname) entry.current = { pathname, local: !routeTransition };
	if (routeTransition || homeTransition) entry.current.local = false;
	const [settledHome, setSettledHome] = useState(home);
	const homeExit = homeTransition && settledHome;
	useEffect(() => {
		if (!homeTransition) setSettledHome(home);
	}, [home, homeTransition]);
	const narrow = useMediaQuery(NARROW_QUERY);
	const mobile = useMediaQuery("(max-width: 760px)");
	const [collapsed, setCollapsed] = useState(() => {
		const stored = localStorage.getItem(SIDEBAR_KEY);
		if (stored) return stored === "1";
		return window.matchMedia(NARROW_QUERY).matches;
	});
	useEffect(() => {
		localStorage.setItem(SIDEBAR_KEY, collapsed ? "1" : "0");
	}, [collapsed]);
	const motion = useSidebarMotion(collapsed);
	const captureMotion = motion.capture;
	const changeCollapsed = useCallback(
		(value: boolean) => {
			captureMotion();
			setStartup(false);
			setCollapsed(value);
		},
		[captureMotion],
	);
	const overlayLayout = settings ? mobile : narrow;
	const overlay = overlayLayout && !collapsed;
	return (
		<SessionsProvider>
			<WorkspaceShell
				pathname={pathname}
				preview={preview}
				collapsed={collapsed}
				settings={settings}
				overlayLayout={overlayLayout}
				overlay={overlay}
				startup={startup}
				initialPage={initialPage}
				home={home}
				homeTransition={homeTransition}
				exportTransition={exportTransition}
				settingsBoundary={settingsBoundary}
				sessionTransition={sessionTransition}
				routeTransition={routeTransition}
				routeEntry={entry.current.local}
				homeExit={homeExit}
				motionRoot={motion.root}
				changeCollapsed={changeCollapsed}
			/>
		</SessionsProvider>
	);
}

const OVERLAY_ESC_GUARD =
	'dialog[open], .session-menu, .popover, [data-slot="popover-dialog"], .summary-scrim, .palette-scrim, .sidebar[data-managing]';
const PALETTE_MODAL_BLOCKERS = "dialog[open], .summary-scrim";
const PALETTE_POPOVER_BLOCKERS =
	'.session-menu, .popover:not([data-exiting="true"]), .select__popover:not([data-exiting="true"]), [data-slot="popover-dialog"]';

type ShellToast = {
	kind: "success" | "warning" | "danger";
	message: string;
	duration: number;
	key: number;
	action?: { label: string; to: string };
};

function WorkspaceShell({
	pathname,
	preview,
	collapsed,
	settings,
	overlayLayout,
	overlay,
	startup,
	initialPage,
	home,
	homeTransition,
	exportTransition,
	settingsBoundary,
	sessionTransition,
	routeTransition,
	routeEntry,
	homeExit,
	motionRoot,
	changeCollapsed,
}: {
	pathname: string;
	preview: boolean;
	collapsed: boolean;
	settings: boolean;
	overlayLayout: boolean;
	overlay: boolean;
	startup: boolean;
	initialPage: boolean;
	home: boolean;
	homeTransition: boolean;
	exportTransition: boolean;
	settingsBoundary: boolean;
	sessionTransition: boolean;
	routeTransition: boolean;
	routeEntry: boolean;
	homeExit: boolean;
	motionRoot: React.RefObject<HTMLDivElement | null>;
	changeCollapsed: (value: boolean) => void;
}) {
	const navigate = useNavigate();
	const sessionsState = useWorkspaceSessions();
	const appActive = !preview && sessionsState.stage === "app_active";
	const activeChannel = !preview && !appActive;
	const appActiveRef = useRef(appActive);
	appActiveRef.current = appActive;
	const channelEpochRef = useRef(0);
	const prevAppActiveRef = useRef(appActive);
	if (prevAppActiveRef.current !== appActive) {
		prevAppActiveRef.current = appActive;
		channelEpochRef.current += 1;
	}
	const activeSessionId = useMemo(() => (preview ? undefined : extractActiveSessionId(pathname)), [pathname, preview]);
	const hasActiveSession = activeSessionId !== undefined;
	const sidebarRef = useRef<HTMLElement | null>(null);
	const collapseButtonRef = useRef<HTMLButtonElement | null>(null);
	const expandButtonRef = useRef<HTMLButtonElement | null>(null);
	const restoreExpandFocusRef = useRef(false);
	const paletteReturnFocusRef = useRef<HTMLElement | null>(null);
	const paletteNavigatedRef = useRef(false);
	const prevPaletteOpenRef = useRef(false);
	const pendingStandbyForkRef = useRef<string | null>(null);
	const expandSidebar = useCallback(() => changeCollapsed(false), [changeCollapsed]);
	const closeSidebar = useCallback(
		(restoreFocus: boolean) => {
			restoreExpandFocusRef.current = restoreFocus;
			changeCollapsed(true);
		},
		[changeCollapsed],
	);
	const shellContext = useMemo(
		() => ({ collapsed, overlay, appActive, expandSidebar, expandButtonRef }),
		[collapsed, overlay, appActive, expandSidebar],
	);

	// 全局双快捷面板（Ctrl+P / Ctrl+O）与全局快捷键状态。
	const [paletteMode, setPaletteMode] = useState<PaletteMode | null>(null);
	const [recentExports, setRecentExports] = useState<RecentExportSummary[]>([]);
	const [shellToast, setShellToast] = useState<ShellToast | null>(null);
	const compactBusyRef = useRef(false);
	const forkBusyRef = useRef(false);
	const exportOpenBusyRef = useRef(false);
	const mountedRef = useRef(true);
	useEffect(() => {
		mountedRef.current = true;
		return () => {
			mountedRef.current = false;
		};
	}, []);
	const showShellToast = useCallback(
		(kind: ShellToast["kind"], message: string, duration = 2600, action?: ShellToast["action"]) => {
			setShellToast((current) => ({ kind, message, duration, action, key: (current?.key ?? 0) + 1 }));
		},
		[],
	);

	useEffect(() => {
		if (appActive) {
			setPaletteMode(null);
			return;
		}
		if (pendingStandbyForkRef.current) {
			const dest = pendingStandbyForkRef.current;
			pendingStandbyForkRef.current = null;
			showShellToast("success", "已创建分支会话", 5000, { label: "打开分支会话", to: dest });
		}
	}, [appActive, showShellToast]);

	// 快捷面板实际由开转关时由外壳统一归还打开前焦点，避免开发模式 StrictMode 重复清理丢失原触发元素。
	useEffect(() => {
		const isOpen = paletteMode !== null && !appActive;
		const wasOpen = prevPaletteOpenRef.current;
		prevPaletteOpenRef.current = isOpen;
		if (!wasOpen || isOpen) return;
		const target = paletteReturnFocusRef.current;
		const navigated = paletteNavigatedRef.current;
		paletteReturnFocusRef.current = null;
		paletteNavigatedRef.current = false;
		if (navigated || appActive || !target || target === document.body || !target.isConnected) return;
		const restore = () => {
			if (document.querySelector(".standby-scrim")) return;
			if (target.isConnected && !target.closest("[inert]")) {
				target.focus({ preventScroll: true });
			}
		};
		restore();
		if (document.activeElement !== target) window.setTimeout(restore, 0);
	}, [paletteMode, appActive]);

	useEffect(() => {
		if (paletteMode !== "quick_open" || !activeChannel) return;
		const controller = new AbortController();
		void apiJson<{ exports?: RecentExportSummary[] }>("/api/exports/recent?limit=20", { signal: controller.signal })
			.then((data) => {
				if (!controller.signal.aborted && Array.isArray(data?.exports)) {
					setRecentExports(data.exports);
				}
			})
			.catch(() => {
				/* 离线或暂无导出记录时保持当前列表，不阻断会话快速切换。 */
			});
		return () => controller.abort();
	}, [paletteMode, activeChannel]);

	useEffect(() => {
		if (!activeChannel) return;
		const onGlobalKeyDown = (event: KeyboardEvent) => {
			if (event.isComposing || event.keyCode === 229) return;
			if (document.querySelector(PALETTE_MODAL_BLOCKERS)) return;
			if (paletteMode === null && document.querySelector(PALETTE_POPOVER_BLOCKERS)) return;
			const key = event.key.toLowerCase();
			const captureReturnFocus = () => {
				if (paletteMode !== null) return;
				paletteNavigatedRef.current = false;
				paletteReturnFocusRef.current =
					document.activeElement instanceof HTMLElement && document.activeElement !== document.body
						? document.activeElement
						: null;
			};
			if (event.ctrlKey && !event.altKey && !event.metaKey && !event.shiftKey) {
				if (key === "p") {
					event.preventDefault();
					event.stopPropagation();
					if (!event.repeat) {
						captureReturnFocus();
						setPaletteMode((current) => (current === "commands" ? null : "commands"));
					}
					return;
				}
				if (key === "o") {
					event.preventDefault();
					event.stopPropagation();
					if (!event.repeat) {
						captureReturnFocus();
						setPaletteMode((current) => (current === "quick_open" ? null : "quick_open"));
					}
					return;
				}
				if (paletteMode !== null) return;
				if (key === "b") {
					event.preventDefault();
					if (!event.repeat) {
						if (!collapsed && overlay) closeSidebar(true);
						else changeCollapsed(!collapsed);
					}
					return;
				}
				if (event.key === "," || event.key === "，" || event.code === "Comma") {
					event.preventDefault();
					if (!event.repeat) {
						if (overlay) closeSidebar(false);
						navigate("/settings", { viewTransition: true });
					}
					return;
				}
			}
			if (paletteMode !== null) return;
			if (event.altKey && !event.ctrlKey && !event.metaKey && !event.shiftKey && key === "n") {
				event.preventDefault();
				if (!event.repeat) {
					if (overlay) closeSidebar(false);
					navigate("/", { viewTransition: true });
				}
			}
		};
		window.addEventListener("keydown", onGlobalKeyDown, true);
		return () => window.removeEventListener("keydown", onGlobalKeyDown, true);
	}, [activeChannel, paletteMode, collapsed, overlay, changeCollapsed, closeSidebar, navigate]);

	const executePaletteTarget = useCallback(
		(target: PaletteTarget) => {
			paletteNavigatedRef.current = willPaletteTargetChangeRoute(target, pathname, activeSessionId);
			setPaletteMode(null);
			if (overlay && target.kind !== "toggle_sidebar") closeSidebar(false);
			switch (target.kind) {
				case "new_session":
					navigate("/", { viewTransition: true });
					return;
				case "all_sessions":
					navigate("/sessions", { viewTransition: true });
					return;
				case "toggle_sidebar":
					if (!collapsed && overlay) closeSidebar(true);
					else changeCollapsed(!collapsed);
					return;
				case "open_settings":
					navigate("/settings", { viewTransition: true });
					return;
				case "open_session":
					navigate(`/s/${encodeURIComponent(target.sessionId)}`, { viewTransition: true });
					return;
				case "slash_export":
					if (activeSessionId)
						navigate(`/s/${encodeURIComponent(activeSessionId)}/export`, { viewTransition: true });
					return;
				case "slash_tree":
					if (activeSessionId)
						navigate(`/s/${encodeURIComponent(activeSessionId)}/tree`, { viewTransition: true });
					return;
				case "slash_compact": {
					if (!activeSessionId) return;
					if (compactBusyRef.current) {
						showShellToast("warning", "正在压缩上下文，请稍候", 2400);
						return;
					}
					const sid = activeSessionId;
					const originEpoch = channelEpochRef.current;
					compactBusyRef.current = true;
					void (async () => {
						try {
							await apiJson(sessionPath(sid, "/compact"), { method: "POST", body: { instructions: "" } });
							if (!mountedRef.current || appActiveRef.current || channelEpochRef.current !== originEpoch) return;
							const livePath = window.location.pathname;
							const expectedPath = `/s/${encodeURIComponent(sid)}`;
							if (extractActiveSessionId(livePath) === sid && livePath !== expectedPath) {
								navigate(expectedPath, { viewTransition: true });
							}
							showShellToast("success", "已完成上下文压缩", 2400);
						} catch (cause) {
							if (mountedRef.current && !appActiveRef.current && channelEpochRef.current === originEpoch) {
								showShellToast("danger", cause instanceof Error ? cause.message : "上下文压缩未完成", 4000);
							}
						} finally {
							compactBusyRef.current = false;
						}
					})();
					return;
				}
				case "slash_fork": {
					if (!activeSessionId) return;
					if (forkBusyRef.current) {
						showShellToast("warning", "正在创建分支会话，请稍候", 2400);
						return;
					}
					const sid = activeSessionId;
					const originPath = window.location.pathname;
					const originEpoch = channelEpochRef.current;
					forkBusyRef.current = true;
					void (async () => {
						try {
							const detail = await apiJson<SessionDetail>(sessionPath(sid));
							if (!mountedRef.current || appActiveRef.current || channelEpochRef.current !== originEpoch) return;
							if (detail.running) {
								showShellToast("warning", "请等待当前回复完成后再分支新会话。", 3200);
								return;
							}
							const targetEntryId = [...(detail.entries ?? [])]
								.reverse()
								.find(
									(entry) =>
										entry.id &&
										entry.message.role === "assistant" &&
										entry.message.stopReason != null &&
										entry.message.stopReason !== "toolUse",
								)?.id;
							if (!targetEntryId) {
								showShellToast("warning", "当前会话尚无可用于分支的已完成回复。", 3200);
								return;
							}
							const created = await apiJson<{ id: string }>(sessionPath(sid, "/fork"), {
								method: "POST",
								body: { entryId: targetEntryId },
							});
							if (!created?.id) throw new Error("后台未返回新会话 ID。");
							if (!mountedRef.current) return;
							const dest = `/s/${encodeURIComponent(created.id)}`;
							if (appActiveRef.current) {
								// 分支在待机期间创建成功：保留新分支地址，待恢复 Web 通道后通过通知提供入口。
								pendingStandbyForkRef.current = dest;
								return;
							}
							if (window.location.pathname === originPath && channelEpochRef.current === originEpoch) {
								navigate(dest, { state: { forkNotice: true } });
							} else {
								showShellToast("success", "已创建分支会话", 5000, { label: "打开分支会话", to: dest });
							}
						} catch (cause) {
							if (mountedRef.current && !appActiveRef.current && channelEpochRef.current === originEpoch) {
								showShellToast("danger", cause instanceof Error ? cause.message : "分支会话失败，请重试", 4500);
							}
						} finally {
							forkBusyRef.current = false;
						}
					})();
					return;
				}
				case "open_export": {
					if (exportOpenBusyRef.current) return;
					const originEpoch = channelEpochRef.current;
					exportOpenBusyRef.current = true;
					void (async () => {
						try {
							await apiJson(`/api/exports/${encodeURIComponent(target.exportId)}/open`, {
								method: "POST",
								body: {},
							});
							if (mountedRef.current && !appActiveRef.current && channelEpochRef.current === originEpoch) {
								showShellToast("success", "已打开导出所在目录", 2400);
							}
						} catch (cause) {
							if (mountedRef.current && !appActiveRef.current && channelEpochRef.current === originEpoch) {
								showShellToast("danger", cause instanceof Error ? cause.message : "无法打开导出目录", 4000);
							}
						} finally {
							exportOpenBusyRef.current = false;
						}
					})();
				}
			}
		},
		[activeSessionId, collapsed, changeCollapsed, closeSidebar, navigate, overlay, pathname, showShellToast],
	);

	// 窄屏浮层抽屉展开时将焦点移入收起钮并限制 Tab 在抽屉内循环；Esc 收起后归还至展开钮。
	useEffect(() => {
		if (!overlay || appActive || paletteMode !== null) return;
		const focusDrawer = () => {
			const drawer = sidebarRef.current;
			if (!drawer || drawer.contains(document.activeElement)) return;
			if (document.querySelector('dialog[open], .session-menu, .popover:not([data-exiting="true"])')) return;
			(collapseButtonRef.current ?? focusableElements(drawer)[0])?.focus({ preventScroll: true });
		};
		focusDrawer();
		const timer = window.setTimeout(focusDrawer, 0);
		const onTabCapture = (event: KeyboardEvent) => {
			if (
				event.key !== "Tab" ||
				event.ctrlKey ||
				event.altKey ||
				event.metaKey ||
				event.isComposing ||
				event.keyCode === 229
			)
				return;
			const drawer = sidebarRef.current;
			if (!drawer) return;
			if (
				document.querySelector(
					'dialog[open], .palette-scrim, .standby-scrim, .session-menu, .popover:not([data-exiting="true"]), [data-slot="popover-dialog"]',
				)
			) {
				return;
			}
			const items = focusableElements(drawer);
			const active =
				document.activeElement instanceof HTMLElement && drawer.contains(document.activeElement)
					? document.activeElement
					: null;
			const target = resolveDialogTabTarget(items, active, event.shiftKey);
			if (!target) return;
			event.preventDefault();
			if (target === "dialog") collapseButtonRef.current?.focus({ preventScroll: true });
			else target.focus({ preventScroll: true });
		};
		const onEscape = (event: KeyboardEvent) => {
			if (event.key !== "Escape" || event.defaultPrevented || event.isComposing || event.keyCode === 229) return;
			if (document.querySelector(OVERLAY_ESC_GUARD)) return;
			event.preventDefault();
			closeSidebar(true);
		};
		window.addEventListener("keydown", onTabCapture, true);
		window.addEventListener("keydown", onEscape);
		return () => {
			window.clearTimeout(timer);
			window.removeEventListener("keydown", onTabCapture, true);
			window.removeEventListener("keydown", onEscape);
		};
	}, [overlay, appActive, paletteMode, closeSidebar]);

	useEffect(() => {
		if (!collapsed || !restoreExpandFocusRef.current) return;
		restoreExpandFocusRef.current = false;
		const focusExpand = () => {
			if (document.querySelector(".standby-scrim, .palette-scrim")) return;
			const btn = expandButtonRef.current;
			if (btn?.isConnected && !btn.closest("[inert]")) btn.focus({ preventScroll: true });
		};
		focusExpand();
		const timer = window.setTimeout(focusExpand, 0);
		return () => window.clearTimeout(timer);
	}, [collapsed]);

	const standbyPresence = usePresenceExit(appActive ? true : null);

	return (
		<AccountProvider enabled={activeChannel}>
			<WorkspaceShellProvider value={shellContext}>
				<NotificationRegion
					contentInset={collapsed || overlayLayout ? 0 : 248}
					inert={overlay || appActive || paletteMode !== null}
				>
					{shellToast && (
						<Notification
							className="palette-toast"
							label={shellToast.message}
							kind={shellToast.kind}
							duration={shellToast.duration}
							lifetimeKey={shellToast.key}
							onDismiss={() => setShellToast(null)}
						>
							{shellToast.kind === "success" ? (
								<CheckCircle2 size={16} aria-hidden="true" />
							) : (
								<CircleAlert size={16} aria-hidden="true" />
							)}
							<span className="notification-message">{shellToast.message}</span>
							{shellToast.action && (
								<button
									type="button"
									className="notice-action"
									onClick={() => {
										const dest = shellToast.action!.to;
										setShellToast(null);
										if (overlay) closeSidebar(false);
										navigate(dest, { viewTransition: true });
									}}
								>
									{shellToast.action.label}
								</button>
							)}
							{shellToast.kind !== "success" && (
								<button
									type="button"
									className="btn-icon xs-24 notification-close"
									aria-label="关闭通知"
									onClick={() => setShellToast(null)}
								>
									<X size={14} aria-hidden="true" />
								</button>
							)}
						</Notification>
					)}
					<div
						ref={motionRoot}
						inert={appActive || paletteMode !== null}
						aria-hidden={appActive || paletteMode !== null || undefined}
						data-startup={(startup && initialPage && !homeTransition && !collapsed) || undefined}
						data-home-startup={(startup && initialPage && home && !homeTransition) || undefined}
						data-startup-origin={(initialPage && home) || undefined}
						data-export-transition={exportTransition || undefined}
						data-home-transition={homeTransition || undefined}
						data-settings-boundary={settingsBoundary || undefined}
						data-session-transition={sessionTransition || undefined}
						data-content-transition={(routeTransition && !homeTransition && !settingsBoundary) || undefined}
						data-route-entry={routeEntry || undefined}
						data-home-exit={homeExit || undefined}
						data-view-transitions={typeof document.startViewTransition === "function"}
						className={`app-layout ${home ? "home-layout" : "chat-layout"} ${settings ? "settings-layout" : ""} ${collapsed ? "sidebar-collapsed" : ""}`}
					>
						<div
							className="sidebar-backdrop tier-3-scrim"
							data-open={overlay ? "true" : "false"}
							aria-hidden="true"
							onClick={() => closeSidebar(true)}
						/>
						<Sidebar
							preview={preview}
							startup={initialPage && !homeTransition && !collapsed}
							collapsed={collapsed}
							sidebarRef={sidebarRef}
							collapseButtonRef={collapseButtonRef}
							onCollapse={() => closeSidebar(overlay)}
							onNavigate={overlay ? () => closeSidebar(false) : undefined}
						/>
						{/* 仅主页无 44px 顶栏时保留左上角浮置展开钮（Q1Ljc / E5nOOw）；非主页统一内联至 44px 页头（MKlBE）。 */}
						{home && collapsed && (
							<button
								ref={expandButtonRef}
								type="button"
								className="sidebar-restore"
								aria-label="展开侧栏"
								title="展开侧栏"
								onClick={expandSidebar}
							>
								<PanelLeft size={18} aria-hidden="true" />
							</button>
						)}
						<div className="workspace-content" inert={overlay || undefined} aria-hidden={overlay || undefined}>
							{home && (
								<div className="home-backdrop" aria-hidden="true">
									<BlueHour kind="home" paused={appActive} />
								</div>
							)}
							<ConnectionStatus enabled={activeChannel} />
							{activeChannel && <MessageOutboxRecovery />}
							<Outlet />
						</div>
					</div>
					{paletteMode !== null && !appActive && (
						<CommandPalette
							mode={paletteMode}
							sessions={sessionsState.sessions}
							activeSessionId={activeSessionId}
							hasActiveSession={hasActiveSession}
							recentExports={recentExports}
							onExecute={executePaletteTarget}
							onDismiss={() => setPaletteMode(null)}
						/>
					)}
					{standbyPresence.mounted &&
						(standbyPresence.exiting ? (
							<div className="standby-scrim-exiting tier-3-scrim" aria-hidden="true">
								<div className="standby-card-exiting tier-3-dialog">
									<div className="dialog-title-row dialog-title-row--info">
										<Globe size={16} aria-hidden="true" />
										<h2>软件端正在使用中</h2>
									</div>
									<p>Web 界面处于待机状态，草稿已保留。从软件端打开 Web 后可继续操作。</p>
									<div className="standby-actions">
										<div className="tier-1-btn-primary">重新检查</div>
									</div>
								</div>
							</div>
						) : (
							<ChannelStandbyOverlay onRefresh={() => sessionsState.refresh()} />
						))}
				</NotificationRegion>
			</WorkspaceShellProvider>
		</AccountProvider>
	);
}
