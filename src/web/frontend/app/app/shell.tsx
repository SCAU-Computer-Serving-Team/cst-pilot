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
	Settings,
	SquarePen,
	SunMoon,
	Trash2,
} from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { Link, useLocation, useNavigate } from "react-router";
import { apiJson, type ProviderStatus } from "./api";
import { BlueHour } from "./blue-hour";
import { Composer, type ComposerConfig } from "./composer";
import { useSessions } from "./web-state";

export const sampleSessions = [
	{ id: "preview", title: "C 盘空间与硬盘信息", group: "今天" },
	{ id: "fan-preview", title: "风扇狂转还降频", group: "今天" },
	{ id: "startup-preview", title: "开机要等三分钟", group: "昨天" },
	{ id: "browser-preview", title: "浏览器经常无响应", group: "一周内" },
];

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

type Theme = "system" | "light" | "dark";
const themeLabels: Record<Theme, string> = { system: "跟随系统", light: "浅色", dark: "深色" };
const nextTheme: Record<Theme, Theme> = { system: "light", light: "dark", dark: "system" };
const currentTheme = (): Theme => {
	const value = document.documentElement.dataset.theme;
	return value === "light" || value === "dark" ? value : "system";
};

export function Sidebar({ preview = false }: { preview?: boolean }) {
	const location = useLocation();
	const state = useSessions();
	const sessions: { id: string; title: string; group?: string; updatedAt?: string }[] = preview
		? sampleSessions
		: state.sessions;
	const groupOf = (session: { group?: string; updatedAt?: string }) =>
		preview ? (session.group ?? "今天") : dayGroup(session.updatedAt);
	const navigate = useNavigate();
	const [providers, setProviders] = useState<ProviderStatus[]>([]);
	const [accountError, setAccountError] = useState("");
	const [accountWorking, setAccountWorking] = useState(false);
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
			selection.style.width = `${target.width}px`;
			selection.style.height = `${target.height}px`;
			selection.style.transform = `translate3d(${target.left - origin.left + top.scrollLeft}px, ${target.top - origin.top + top.scrollTop}px, 0)`;
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
		if (preview) return;
		const events = new EventSource("/api/events");
		events.addEventListener("state", (event) => {
			void refreshAccount();
			try {
				if (JSON.parse((event as MessageEvent).data)?.type === "settings_changed") void refreshTheme();
			} catch {
				/* 其他状态事件不影响当前主题。 */
			}
		});
		events.addEventListener("reset", () => {
			void refreshAccount();
			void refreshTheme();
		});
		return () => events.close();
	}, [preview, refreshAccount, refreshTheme]);
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
		setAccountWorking(true);
		try {
			const active = providers.filter((provider) => provider.type);
			const results = await Promise.allSettled(
				active.map((provider) =>
					apiJson(`/api/auth/${encodeURIComponent(provider.id)}/logout`, { method: "POST" }),
				),
			);
			await refreshAccount();
			if (results.some((result) => result.status === "rejected")) setAccountError("部分模型服务退出失败，请重试");
		} catch (cause) {
			setAccountError(cause instanceof Error ? cause.message : "退出登录失败");
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
		<aside className="sidebar" aria-label="会话导航">
			<div className="sidebar-top" ref={topRef}>
				<span className="sidebar-selection" ref={selectionRef} aria-hidden="true" />
				<div className="sidebar-brand">
					<Link className="product-name" to={preview ? "/?preview=1" : "/"}>
						CST Pilot
					</Link>
					<Button variant="ghost" isIconOnly isDisabled className="sidebar-icon" aria-label="会话搜索尚未开放">
						<PanelLeft size={22} />
					</Button>
				</div>
				<Link
					className={`sidebar-new ${location.pathname === "/" ? "selected" : ""}`}
					to={preview ? "/?preview=1" : "/"}
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
					<span>{preview ? "Tim2354" : "当前电脑"}</span>
					<small>
						{preview ? "24宣传指导 · 设计预览" : state.stage === "preview" ? "页面预览模式" : "本机运行中"}
					</small>
				</div>
				<Popover>
					<Button variant="ghost" isIconOnly className="sidebar-account-trigger" aria-label="账号菜单">
						<Settings size={22} aria-hidden="true" />
					</Button>
					<Popover.Content placement="top end" className="sidebar-account-popover">
						<Popover.Dialog aria-label="账号菜单" className="sidebar-account-menu">
							{providers.some((provider) => provider.type) ? (
								<>
									<Link className="sidebar-account-item" to="/settings#accounts">
										<CircleUserRound size={18} aria-hidden="true" />
										账号信息
									</Link>
									<Link className="sidebar-account-item" to="/settings">
										<Settings size={18} aria-hidden="true" />
										设置
									</Link>
								</>
							) : (
								<Link className="sidebar-account-item" to="/login">
									<LogIn size={18} aria-hidden="true" />
									登录
								</Link>
							)}
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
								href="https://github.com/SCAU-Computer-Serving-Team/cst-pilot/issues/new"
								target="_blank"
								rel="noopener noreferrer"
							>
								<MessageSquareWarning size={18} aria-hidden="true" />
								反馈问题
							</a>
							{providers.some((provider) => provider.type) && (
								<>
									<hr className="session-menu-divider" />
									<Button
										variant="ghost"
										className="sidebar-account-item"
										isDisabled={accountWorking}
										onPress={() => void logout()}
									>
										<LogOut size={18} aria-hidden="true" />
										登出
									</Button>
								</>
							)}
							{accountError && (
								<p role="alert" className="sidebar-error">
									{accountError}
								</p>
							)}
						</Popover.Dialog>
					</Popover.Content>
				</Popover>
			</div>
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

export function HomeSurface({ preview = false }: { preview?: boolean }) {
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
		navigate(`/s/${session.id}`);
	}
	return (
		<main className="home-main">
			<BlueHour kind="home" />
			<div className="home-center">
				<div className="home-greeting">
					<BrandArcs />
					<h1>我该如何协助您？</h1>
				</div>
				<Composer home onSend={preview ? undefined : send} />
			</div>
			<span className="home-footnote">@cst-pilot-web</span>
		</main>
	);
}
