import { Button, Disclosure, Popover } from "@heroui/react";
import {
	CircleUserRound,
	LayoutDashboard,
	LogIn,
	MessageSquareWarning,
	Monitor,
	PanelLeft,
	Power,
	Settings,
	SquarePen,
	SunMoon,
} from "lucide-react";
import { type CSSProperties, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { Link, PrefetchPageLinks, useLocation, useNavigate, useNavigation } from "react-router";
import { describeAccount } from "../account/format";
import { useAccount } from "../account/use-account";
import { accountProvider, loginPath } from "../auth/providers";
import { Composer, type ComposerConfig } from "../composer/composer";
import { apiJson, type ProviderStatus } from "../data/api";
import { attachTelemetryHints } from "../data/telemetry-hints";
import { useGlobalEvents } from "../data/web-state";
import { BlueHour } from "./blue-hour";
import { ExitDialog } from "./exit-dialog";
import { usePanelDismiss } from "./panel-dismiss";
import { sampleSessions } from "./sample-sessions";
import { SessionActionsOverlay, useSessionActions } from "./session-actions";
import { SessionEntry } from "./session-entry";
import { orderedSessions, RECENT_SESSION_LIMIT } from "./session-list";
import { SessionManagementToolbar } from "./session-management";
import { useWorkspaceSessions } from "./sessions-context";
import { currentTheme, nextTheme, type Theme, themeLabels } from "./theme";

/** 点击时直接启动选区移动，路由提交与取消仍由 React Router 校正。 */
function moveSidebarSelection(top: HTMLElement, selection: HTMLElement, active: HTMLElement, animate: boolean) {
	const origin = top.getBoundingClientRect();
	const target = active.getBoundingClientRect();
	const transform = `translate3d(0px, ${target.top - origin.top + top.scrollTop}px, 0px)`;
	// 宽度变化与重复测量不取消仍在播放的滑动。
	const height = `${target.height}px`;
	if (
		selection.style.transform === transform &&
		selection.style.height === height &&
		selection.style.visibility === "visible"
	)
		return;
	if (!animate) selection.style.transition = "none";
	selection.style.transform = transform;
	selection.style.height = height;
	selection.style.visibility = "visible";
	if (!animate) {
		void selection.offsetWidth;
		selection.style.transition = "";
	}
	top.dataset.selectionReady = "true";
}

export function Sidebar({
	preview = false,
	startup = false,
	collapsed = false,
	onCollapse,
	onNavigate,
}: {
	preview?: boolean;
	startup?: boolean;
	collapsed?: boolean;
	onCollapse: () => void;
	onNavigate?: () => void;
}) {
	useEffect(() => {
		if (!preview) return attachTelemetryHints(window);
	}, [preview]);
	const location = useLocation();
	const navigation = useNavigation();
	const selectionPath = navigation.location?.pathname ?? location.pathname;
	const state = useWorkspaceSessions();
	const [recentOpen, setRecentOpen] = useState(true);
	const startupPending = useRef(startup);
	const [startupRows, setStartupRows] = useState(false);
	useLayoutEffect(() => {
		if (!startup) {
			startupPending.current = false;
			setStartupRows(false);
			return;
		}
		if (!startupPending.current || !state.stage) return;
		startupPending.current = false;
		setStartupRows(true);
		const timer = window.setTimeout(() => setStartupRows(false), 1100);
		return () => window.clearTimeout(timer);
	}, [startup, state.stage]);
	const sessions = useMemo(
		() => orderedSessions(preview ? sampleSessions : state.sessions).slice(0, RECENT_SESSION_LIMIT),
		[preview, state.sessions],
	);
	const [providers, setProviders] = useState<ProviderStatus[]>([]);
	const team = accountProvider(providers);
	const teamSignedIn = team?.type === "oauth" || team?.type === "api_key";
	const [accountError, setAccountError] = useState("");
	const [accountWorking, setAccountWorking] = useState(false);
	const [accountMenuOpen, setAccountMenuOpen] = useState(false);
	const [exitOpen, setExitOpen] = useState(false);
	// 账号菜单只显示身份；打开时按缓存时效同步。
	const { account: cstoaAccount, refresh: refreshCstoaAccount } = useAccount();
	const cstoaSummary = describeAccount(cstoaAccount);
	const teamRequiresLogin = team?.requiresLogin || cstoaAccount?.requiresLogin;
	const accountTriggerRef = useRef<HTMLButtonElement>(null);
	usePanelDismiss(accountMenuOpen, () => setAccountMenuOpen(false), accountTriggerRef);
	function closeAccountMenu() {
		setAccountMenuOpen(false);
		onNavigate?.();
	}
	const [theme, setTheme] = useState<Theme>(currentTheme);
	const topRef = useRef<HTMLDivElement>(null);
	const selectionRef = useRef<HTMLSpanElement>(null);
	const focusSession = useCallback((id?: string) => {
		requestAnimationFrame(() => {
			const row = id
				? topRef.current?.querySelector<HTMLElement>(`[data-session-id="${CSS.escape(id)}"]`)
				: topRef.current?.querySelector<HTMLElement>(".sidebar-new");
			(row?.querySelector<HTMLElement>("input") ?? row)?.focus();
		});
	}, []);
	const actions = useSessionActions(sessions, focusSession, preview);
	const { management, setMenu, openSessionMenu } = actions;
	const finishManagement = management.finish;
	useEffect(() => {
		if (collapsed) {
			setMenu(null);
			finishManagement();
		}
	}, [collapsed, finishManagement, setMenu]);
	// biome-ignore lint/correctness/useExhaustiveDependencies: 列表与导航完成/取消后重新测量，含同路径导航
	useLayoutEffect(() => {
		const top = topRef.current;
		const selection = selectionRef.current;
		const active = management.managing
			? null
			: Array.from(
					top?.querySelectorAll<HTMLAnchorElement>("a.sidebar-new, a.sidebar-session, a.sidebar-all") ?? [],
				).find((row) => new URL(row.href).pathname === selectionPath);
		if (!top || !selection) return;
		if (!active || (!recentOpen && active.classList.contains("sidebar-session"))) {
			delete top.dataset.selectionReady;
			selection.style.visibility = "hidden";
			return;
		}
		const move = (animate: boolean) => moveSidebarSelection(top, selection, active, animate);
		move(top.dataset.selectionReady === "true");
		const observer = new ResizeObserver(() => move(false));
		observer.observe(top);
		observer.observe(active);
		const panel = top.querySelector(".sidebar-recent .disclosure__content");
		if (panel) observer.observe(panel);
		window.addEventListener("resize", moveOnResize);
		function moveOnResize() {
			move(false);
		}
		return () => {
			observer.disconnect();
			window.removeEventListener("resize", moveOnResize);
		};
	}, [selectionPath, location.key, navigation.state, sessions, management.managing, recentOpen]);
	const refreshAccount = useCallback(async () => {
		if (preview) return;
		try {
			const data = await apiJson<{ providers: ProviderStatus[] }>("/api/auth");
			setProviders(data.providers);
			setAccountError("");
		} catch (cause) {
			setAccountError(cause instanceof Error ? cause.message : "无法读取登录状态");
		}
	}, [preview]);
	const refreshTheme = useCallback(async () => {
		if (preview) return;
		try {
			const settings = await apiJson<{ theme: Theme }>("/api/settings");
			document.documentElement.dataset.theme = settings.theme;
			setTheme(settings.theme);
		} catch {
			/* 页面仍使用当前主题；主题按钮提交失败时另行提示。 */
		}
	}, [preview]);
	useEffect(() => {
		const root = document.documentElement;
		const observer = new MutationObserver(() => setTheme(currentTheme()));
		observer.observe(root, { attributes: true, attributeFilter: ["data-theme"] });
		return () => observer.disconnect();
	}, []);
	useEffect(() => {
		void refreshAccount();
		void refreshTheme();
	}, [refreshAccount, refreshTheme]);
	useGlobalEvents((name, event) => {
		if (name === "reset") {
			void refreshAccount();
			void refreshTheme();
			return;
		}
		if (name !== "state") return;
		try {
			const type = JSON.parse(event?.data ?? "null")?.type;
			if (type === "auth_changed") void refreshAccount();
			if (type === "settings_changed") void refreshTheme();
		} catch {
			/* 其他状态事件不影响当前主题。 */
		}
	}, !preview);
	async function changeTheme() {
		const value = nextTheme[theme];
		if (preview) {
			document.documentElement.dataset.theme = value;
			setTheme(value);
			return;
		}
		setAccountWorking(true);
		try {
			await apiJson("/api/settings", { method: "PATCH", body: { theme: value } });
			document.documentElement.dataset.theme = value;
			setTheme(value);
			setAccountError("");
		} catch (cause) {
			setAccountError(cause instanceof Error ? cause.message : "主题切换失败，请重试");
		} finally {
			setAccountWorking(false);
		}
	}
	return (
		<aside
			className="sidebar"
			aria-label="会话导航"
			data-managing={management.managing || undefined}
			data-startup-rows={startupRows || undefined}
			inert={collapsed}
			aria-hidden={collapsed}
		>
			{/* 所有会话共用聊天路由，只插入一份预载声明。 */}
			{sessions[0] && <PrefetchPageLinks page={`/s/${sessions[0].id}${preview ? "?preview=1" : ""}`} />}
			<div className="sidebar-top" ref={topRef}>
				<span className="sidebar-selection" ref={selectionRef} aria-hidden="true" />
				<div className="sidebar-brand">
					<Link className="product-name" to={preview ? "/?preview=1" : "/"} viewTransition>
						CST Pilot
					</Link>
					<Button variant="ghost" isIconOnly className="sidebar-icon" aria-label="收起侧栏" onPress={onCollapse}>
						<PanelLeft size={22} />
					</Button>
				</div>
				<div className="sidebar-new-area">
					<Link
						className={`sidebar-new ${!management.managing && location.pathname === "/" ? "selected" : ""}`}
						to={preview ? "/?preview=1" : "/"}
						viewTransition
						onClick={onNavigate}
					>
						<span className="sidebar-nav-icon">
							<SquarePen size={18} aria-hidden="true" />
						</span>
						新对话
					</Link>
					<SessionManagementToolbar
						active={management.managing}
						allSelected={management.allSelected}
						busy={management.busy}
						onToggleAll={management.toggleAll}
						onFinish={() => {
							setMenu(null);
							management.finish();
							topRef.current?.querySelector<HTMLElement>(".sidebar-new")?.focus();
						}}
					/>
				</div>
				<button type="button" className="sidebar-new sidebar-dashboard" disabled title="仪表盘功能计划中">
					<span className="sidebar-nav-icon">
						<LayoutDashboard size={18} aria-hidden="true" />
					</span>
					仪表盘
				</button>
				<Disclosure
					className="sidebar-recent"
					isExpanded={recentOpen}
					onExpandedChange={(open) => {
						setMenu(null);
						if (!open) finishManagement();
						setRecentOpen(open);
					}}
				>
					<Disclosure.Heading>
						<Disclosure.Trigger
							className="sidebar-recent-heading"
							style={{ "--startup-row": 0 } as CSSProperties}
						>
							最近
							<Disclosure.Indicator />
						</Disclosure.Trigger>
					</Disclosure.Heading>
					<Disclosure.Content inert={!recentOpen} aria-hidden={!recentOpen}>
						<div className="sidebar-recent-list">
							{sessions.map((session, index) => (
								<SessionEntry
									key={session.id}
									session={session}
									index={index}
									active={
										location.pathname === `/s/${session.id}` ||
										location.pathname.startsWith(`/s/${session.id}/`)
									}
									managing={management.managing}
									selected={management.selection?.has(session.id) ?? false}
									disabled={management.busy}
									preview={preview}
									viewTransition={
										location.pathname.startsWith("/settings") || location.pathname.endsWith("/export")
									}
									onToggle={() => {
										setMenu(null);
										management.toggle(session.id);
									}}
									onNavigate={(event) => {
										if (
											event.defaultPrevented ||
											event.button !== 0 ||
											event.metaKey ||
											event.ctrlKey ||
											event.shiftKey ||
											event.altKey
										)
											return;
										if (topRef.current && selectionRef.current)
											moveSidebarSelection(topRef.current, selectionRef.current, event.currentTarget, true);
										onNavigate?.();
									}}
									onContextMenu={(event) => {
										event.preventDefault();
										openSessionMenu(session, event.clientX, event.clientY);
									}}
								/>
							))}
							{!sessions.length && (state.stage || preview) && <p className="sidebar-empty">暂无会话</p>}
						</div>
					</Disclosure.Content>
				</Disclosure>
				<Link
					className={`sidebar-all ${location.pathname === "/sessions" ? "selected" : ""}`}
					to={preview ? "/sessions?preview=1" : "/sessions"}
					aria-current={location.pathname === "/sessions" ? "page" : undefined}
					viewTransition
					onClick={onNavigate}
				>
					查看全部
				</Link>
				{state.error && (
					<p className="sidebar-error" role="alert">
						{state.error}
					</p>
				)}
			</div>
			<SessionActionsOverlay actions={actions} />
			<div className="sidebar-footer">
				<div className="device-icon">
					<Monitor size={18} />
				</div>
				<div className="device-copy">
					<span>{preview ? "Tim2354" : teamSignedIn ? "CSTOA 账号" : "当前电脑"}</span>
					<small>
						{preview
							? "24宣传指导 · 设计预览"
							: teamSignedIn
								? teamRequiresLogin
									? "需要重新登录"
									: "已登录"
								: state.stage === "preview"
									? "页面预览模式"
									: "CSTOA 未登录"}
					</small>
				</div>
				<Popover
					isOpen={accountMenuOpen}
					onOpenChange={(open) => {
						setAccountMenuOpen(open);
						if (open) void refreshCstoaAccount();
					}}
				>
					<Button
						ref={accountTriggerRef}
						variant="ghost"
						isIconOnly
						className="sidebar-account-trigger"
						aria-label="账号菜单"
					>
						<Settings size={22} aria-hidden="true" />
					</Button>
					<Popover.Content placement="right bottom" offset={20} className="sidebar-account-popover">
						<Popover.Dialog aria-label="账号菜单" className="sidebar-account-menu">
							{cstoaSummary && (
								<>
									<div className="sidebar-account-identity">
										<strong title={cstoaSummary.identity}>{cstoaSummary.identity}</strong>
									</div>
									<hr className="session-menu-divider" />
								</>
							)}
							<Link className="sidebar-account-item" to="/account" viewTransition onClick={closeAccountMenu}>
								<CircleUserRound size={18} aria-hidden="true" />
								账号信息
							</Link>
							{(!teamSignedIn || teamRequiresLogin) && (
								<Link
									className="sidebar-account-item"
									to={loginPath("cstoa", "oauth", "/account")}
									viewTransition={location.pathname.endsWith("/export")}
									onClick={closeAccountMenu}
								>
									<LogIn size={18} aria-hidden="true" />
									{teamRequiresLogin ? "重新登录 CSTOA" : "登录 CSTOA"}
								</Link>
							)}
							<Link className="sidebar-account-item" to="/settings" viewTransition onClick={closeAccountMenu}>
								<Settings size={18} aria-hidden="true" />
								设置
							</Link>
							<Button
								variant="ghost"
								className="sidebar-account-item"
								isDisabled={accountWorking}
								aria-label={`主题，当前${themeLabels[theme]}，切换为${themeLabels[nextTheme[theme]]}`}
								onPress={() => void changeTheme()}
							>
								<SunMoon size={18} aria-hidden="true" />
								<span>主题</span>
								<small className="sidebar-theme-value">{themeLabels[theme]}</small>
							</Button>
							<a
								className="sidebar-account-item"
								href="https://github.com/SCAU-CST/cst-pilot/issues/new"
								onClick={closeAccountMenu}
								target="_blank"
								rel="noopener noreferrer"
							>
								<MessageSquareWarning size={18} aria-hidden="true" />
								反馈问题
							</a>
							<hr className="session-menu-divider" />
							<Button
								variant="ghost"
								className="sidebar-account-item"
								onPress={() => {
									closeAccountMenu();
									setExitOpen(true);
								}}
							>
								<Power size={18} aria-hidden="true" />
								退出 CST Pilot
							</Button>
							{accountError && (
								<p role="alert" className="sidebar-error">
									{accountError}
								</p>
							)}
						</Popover.Dialog>
					</Popover.Content>
				</Popover>
			</div>
			{exitOpen && <ExitDialog onClose={() => setExitOpen(false)} />}
		</aside>
	);
}

export function BrandArcs() {
	return (
		<svg className="brand-arcs" viewBox="0 0 400 400" fill="none" aria-hidden="true">
			<defs>
				<linearGradient id="brand-arc-gradient" x1="0%" y1="100%" x2="0%" y2="0%">
					<stop stopColor="#fff" stopOpacity="0" />
					<stop offset="55%" stopColor="#fff" stopOpacity=".12" />
					<stop offset="100%" stopColor="#fff" />
				</linearGradient>
			</defs>
			<path d="M200 200 L340 60 A197.5 197.5 0 1 0 60 340 Z" stroke="url(#brand-arc-gradient)" strokeWidth="5" />
			<path d="M200 200 L323 77 A171.5 171.5 0 1 0 77 323 Z" stroke="url(#brand-arc-gradient)" strokeWidth="5" />
		</svg>
	);
}

export function HomeSurface({
	preview = false,
	paused = false,
	externalBackground = false,
}: {
	preview?: boolean;
	paused?: boolean;
	externalBackground?: boolean;
}) {
	const navigate = useNavigate();
	async function send(text: string, config: ComposerConfig) {
		const session = await apiJson<{ id: string }>("/api/sessions", {
			method: "POST",
			body: config.provider
				? { provider: config.provider, modelId: config.modelId, thinkingLevel: config.thinkingLevel }
				: {},
			idempotencyKey: config.messageId,
		});
		await apiJson(`/api/sessions/${encodeURIComponent(session.id)}/messages`, {
			method: "POST",
			idempotencyKey: config.messageId,
			body: {
				id: config.messageId,
				text,
				delivery: "queue",
				images: config.images,
				...(config.skill ? { skill: config.skill } : {}),
			},
		});
		navigate(`/s/${session.id}`, { viewTransition: true });
	}
	return (
		<main className={`home-main ${externalBackground ? "home-main--external-background" : ""}`}>
			{!externalBackground && (
				<div className="home-backdrop" aria-hidden="true">
					<BlueHour kind="home" paused={paused} />
				</div>
			)}
			<div className="home-center">
				<div className="home-greeting">
					<BrandArcs />
					<h1>我该如何协助您？</h1>
				</div>
				<Composer home onSend={preview || paused ? undefined : send} />
			</div>
			<span className="home-footnote">@cst-pilot-web</span>
		</main>
	);
}
