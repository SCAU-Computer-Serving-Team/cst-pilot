import { Button, Popover } from "@heroui/react";
import {
	CircleUserRound,
	Download,
	LogIn,
	LogOut,
	MessageSquareWarning,
	Monitor,
	PanelLeft,
	Pencil,
	Power,
	Settings,
	SquarePen,
	SunMoon,
	Trash2,
} from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { Link, useLocation, useNavigate } from "react-router";
import { describeAccount } from "../account/format";
import { useAccount } from "../account/use-account";
import { accountProvider, loginPath } from "../auth/providers";
import { Composer, type ComposerConfig } from "../composer/composer";
import { apiJson, type ProviderStatus } from "../data/api";
import { useGlobalEvents, useSessions } from "../data/web-state";
import { BlueHour } from "./blue-hour";
import { ExitDialog } from "./exit-dialog";
import { usePanelDismiss } from "./panel-dismiss";
import { sampleSessions } from "./sample-sessions";
import { currentTheme, nextTheme, type Theme, themeLabels } from "./theme";

// 画布按日期分组会话列表。
const sessionGroups = ["今天", "昨天", "一周内", "更早"];
const dayGroup = (value?: string) => {
	const target = value ? new Date(value) : undefined;
	if (!target || Number.isNaN(target.getTime())) return "更早";
	const today = new Date();
	today.setHours(0, 0, 0, 0);
	const day = new Date(target);
	day.setHours(0, 0, 0, 0);
	const days = Math.round((today.getTime() - day.getTime()) / 86400000);
	return days <= 0 ? "今天" : days === 1 ? "昨天" : days <= 7 ? "一周内" : "更早";
};

export function Sidebar({
	preview = false,
	collapsed = false,
	onCollapse,
	onNavigate,
}: {
	preview?: boolean;
	collapsed?: boolean;
	onCollapse: () => void;
	onNavigate?: () => void;
}) {
	const location = useLocation();
	const state = useSessions();
	const sessions: { id: string; title: string; group?: string; updatedAt?: string }[] = preview
		? sampleSessions
		: state.sessions;
	const groupOf = (session: { group?: string; updatedAt?: string }) =>
		preview ? (session.group ?? "今天") : dayGroup(session.updatedAt);
	const navigate = useNavigate();
	const [providers, setProviders] = useState<ProviderStatus[]>([]);
	const team = accountProvider(providers);
	const teamSignedIn = team?.type === "oauth";
	const [accountError, setAccountError] = useState("");
	const [accountWorking, setAccountWorking] = useState(false);
	const [accountMenuOpen, setAccountMenuOpen] = useState(false);
	const [exitOpen, setExitOpen] = useState(false);
	// 账号菜单顶部显示本人姓名与剩余额度点；打开菜单时重取一次。
	const { account: cstoaAccount, refresh: refreshCstoaAccount } = useAccount();
	const cstoaSummary = describeAccount(cstoaAccount);
	usePanelDismiss(accountMenuOpen, () => setAccountMenuOpen(false));
	function closeAccountMenu() {
		setAccountMenuOpen(false);
		onNavigate?.();
	}
	const [theme, setTheme] = useState<Theme>(currentTheme);
	const [menu, setMenu] = useState<{ id: string; title: string; x: number; y: number } | null>(null);
	const menuRef = useRef<HTMLDivElement>(null);
	const topRef = useRef<HTMLDivElement>(null);
	const selectionRef = useRef<HTMLSpanElement>(null);
	// biome-ignore lint/correctness/useExhaustiveDependencies: 选区高亮的位置跟随路由与列表内容，二者不在 effect 体内读取
	useLayoutEffect(() => {
		const top = topRef.current;
		const selection = selectionRef.current;
		const active = top?.querySelector<HTMLElement>(".sidebar-new.selected, .sidebar-session.selected");
		if (!top || !selection) return;
		if (!active) {
			delete top.dataset.selectionReady;
			selection.style.visibility = "hidden";
			return;
		}
		const move = (animate: boolean) => {
			if (!animate) selection.style.transition = "none";
			const origin = top.getBoundingClientRect();
			const target = active.getBoundingClientRect();
			selection.style.transform = `translate3d(0, ${target.top - origin.top + top.scrollTop}px, 0)`;
			selection.style.visibility = "visible";
			if (!animate) {
				void selection.offsetWidth;
				selection.style.transition = "";
			}
		};
		move(top.dataset.selectionReady === "true");
		top.dataset.selectionReady = "true";
		let topWidth = top.clientWidth;
		let activeWidth = active.offsetWidth;
		let activeHeight = active.offsetHeight;
		const observer = new ResizeObserver(() => {
			if (top.clientWidth === topWidth && active.offsetWidth === activeWidth && active.offsetHeight === activeHeight)
				return;
			topWidth = top.clientWidth;
			activeWidth = active.offsetWidth;
			activeHeight = active.offsetHeight;
			move(false);
		});
		observer.observe(top);
		observer.observe(active);
		window.addEventListener("resize", moveOnResize);
		function moveOnResize() {
			move(false);
		}
		return () => {
			observer.disconnect();
			window.removeEventListener("resize", moveOnResize);
		};
	}, [location.pathname, sessions]);
	useEffect(() => {
		if (!menu) return;
		// 右键菜单只做一个动作，点击别处、按 Esc、滚动都关闭。
		const close = (event: Event) => {
			if (!menuRef.current?.contains(event.target as Node)) setMenu(null);
		};
		const closeOnEscape = (event: KeyboardEvent) => {
			if (event.key === "Escape") setMenu(null);
		};
		const dismiss = () => setMenu(null);
		menuRef.current?.querySelector<HTMLElement>('[role="menuitem"]')?.focus();
		window.addEventListener("pointerdown", close);
		window.addEventListener("keydown", closeOnEscape);
		window.addEventListener("scroll", dismiss, true);
		return () => {
			window.removeEventListener("pointerdown", close);
			window.removeEventListener("keydown", closeOnEscape);
			window.removeEventListener("scroll", dismiss, true);
		};
	}, [menu]);
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
	async function logout() {
		if (!teamSignedIn || accountWorking) return;
		setAccountWorking(true);
		try {
			await apiJson("/api/auth/cstoa/logout", { method: "POST" });
			await refreshAccount();
		} catch (cause) {
			setAccountError(cause instanceof Error ? cause.message : "退出账号失败，请重试");
		} finally {
			setAccountWorking(false);
		}
	}
	async function remove(id: string) {
		setMenu(null);
		if (!window.confirm("删除此会话及其图片？此操作无法撤销。")) return;
		try {
			await apiJson(`/api/sessions/${encodeURIComponent(id)}`, { method: "DELETE" });
			if (location.pathname.startsWith(`/s/${id}`)) navigate("/");
			await state.refresh();
		} catch (cause) {
			window.alert(cause instanceof Error ? cause.message : "删除失败");
		}
	}
	return (
		<aside className="sidebar" aria-label="会话导航" inert={collapsed} aria-hidden={collapsed}>
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
				<Link
					className={`sidebar-new ${location.pathname === "/" ? "selected" : ""}`}
					to={preview ? "/?preview=1" : "/"}
					viewTransition
					onClick={onNavigate}
				>
					<SquarePen size={18} />
					新对话
				</Link>
				{sessions.length > 0 ? (
					sessionGroups.map((group) => {
						const list = sessions.filter((session) => groupOf(session) === group);
						return list.length === 0 ? null : (
							<section className="sidebar-group" key={group} aria-label={group}>
								<h2>{group}</h2>
								{list.map((session) => (
									<div className="sidebar-session-row" key={session.id}>
										<Link
											className={`sidebar-session ${location.pathname === `/s/${session.id}` ? "selected" : ""}`}
											to={`/s/${session.id}${preview ? "?preview=1" : ""}`}
											viewTransition
											onClick={onNavigate}
											onContextMenu={
												preview
													? undefined
													: (event) => {
															event.preventDefault();
															setMenu({
																id: session.id,
																title: session.title,
																x: event.clientX,
																y: event.clientY,
															});
														}
											}
										>
											<span>{session.title}</span>
										</Link>
									</div>
								))}
							</section>
						);
					})
				) : (
					<p className="sidebar-empty">暂无会话</p>
				)}
				{state.error && (
					<p className="sidebar-error" role="alert">
						{state.error}
					</p>
				)}
			</div>
			{menu && (
				<div
					className="session-menu"
					ref={menuRef}
					role="menu"
					aria-label={`会话 ${menu.title}`}
					style={{
						left: Math.max(8, Math.min(menu.x, window.innerWidth - 248)),
						top: Math.max(8, Math.min(menu.y, window.innerHeight - 177)),
					}}
				>
					<button
						type="button"
						role="menuitem"
						className="sidebar-account-item"
						onClick={() => void rename(menu.id, menu.title)}
					>
						<Pencil size={18} aria-hidden="true" />
						重命名
					</button>
					<a
						role="menuitem"
						className="sidebar-account-item"
						href={`/api/sessions/${encodeURIComponent(menu.id)}/export`}
						download
						onClick={() => setMenu(null)}
					>
						<Download size={18} aria-hidden="true" />
						导出
					</a>
					<hr className="session-menu-divider" />
					<button
						type="button"
						role="menuitem"
						className="sidebar-account-item sidebar-menu-delete"
						onClick={() => void remove(menu.id)}
					>
						<Trash2 size={18} aria-hidden="true" />
						删除
					</button>
				</div>
			)}
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
								? team?.requiresLogin
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
					<Button variant="ghost" isIconOnly className="sidebar-account-trigger" aria-label="账号菜单">
						<Settings size={22} aria-hidden="true" />
					</Button>
					<Popover.Content placement="top end" className="sidebar-account-popover">
						<Popover.Dialog aria-label="账号菜单" className="sidebar-account-menu">
							{cstoaSummary && (
								<>
									<div className="sidebar-account-identity">
										<strong>{cstoaSummary.identity}</strong>
										<small>{cstoaSummary.quota}</small>
									</div>
									<hr className="session-menu-divider" />
								</>
							)}
							<Link className="sidebar-account-item" to="/account" onClick={closeAccountMenu}>
								<CircleUserRound size={18} aria-hidden="true" />
								账号信息
							</Link>
							{(!teamSignedIn || team?.requiresLogin) && (
								<Link
									className="sidebar-account-item"
									to={loginPath("cstoa", "oauth", "/account")}
									onClick={closeAccountMenu}
								>
									<LogIn size={18} aria-hidden="true" />
									{team?.requiresLogin ? "重新登录 CSTOA" : "登录 CSTOA"}
								</Link>
							)}
							<Link className="sidebar-account-item" to="/settings" onClick={closeAccountMenu}>
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
							{teamSignedIn && (
								<>
									<hr className="session-menu-divider" />
									<Button
										variant="ghost"
										className="sidebar-account-item"
										isDisabled={accountWorking}
										onPress={() => {
											closeAccountMenu();
											void logout();
										}}
									>
										<LogOut size={18} aria-hidden="true" />
										退出 CSTOA
									</Button>
								</>
							)}
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
