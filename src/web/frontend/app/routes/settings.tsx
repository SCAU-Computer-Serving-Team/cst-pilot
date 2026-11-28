import "../settings/settings.css";
import { Settings as SettingsIcon } from "lucide-react";
import { type KeyboardEvent as ReactKeyboardEvent, useCallback, useEffect, useRef, useState } from "react";
import { Outlet, useLocation, useNavigate } from "react-router";
import { useGlobalEvents } from "../data/web-state";
import { SettingsFeedback, SettingsLink } from "../settings/settings-frame";
import { invalidateSetting, preloadSettings, settingsPaths } from "../settings/settings-resource-cache";
import { usePointerMutex } from "../shell/kbd-focus";
import { resolveToolbarNavKey } from "../shell/keyboard-navigation";
import { SidebarHeaderToggle } from "../shell/workspace-shell-context";

const tabs = [
	{ to: "/settings/general", label: "常规" },
	{ to: "/settings", label: "提供商" },
	{ to: "/settings/models", label: "模型偏好" },
	{ to: "/settings/shortcuts", label: "快捷键" },
];
export default function Settings() {
	const { pathname } = useLocation();
	const navigate = useNavigate();
	const cleanPath = pathname.length > 1 ? pathname.replace(/\/+$/, "") : pathname;
	const activePath = ["/settings/add", "/settings/provider", "/settings/custom"].includes(cleanPath)
		? "/settings"
		: cleanPath;
	const index = tabs.findIndex((tab) => tab.to === activePath);
	const tabStopIdx = index >= 0 ? index : 1;
	const [navigation, setNavigation] = useState({ pathname, index, direction: 0 });
	if (navigation.pathname !== pathname) {
		setNavigation({
			pathname,
			index,
			direction: index < 0 || navigation.index < 0 ? 0 : Math.sign(index - navigation.index),
		});
	}
	const navRef = useRef<HTMLElement>(null);
	const pagesRef = useRef<HTMLDivElement>(null);
	const routeRef = useRef<HTMLDivElement>(null);
	const [navKbdActive, setNavKbdActive] = useState(false);
	const clearNavKbd = useCallback(() => setNavKbdActive(false), []);
	usePointerMutex(navKbdActive, clearNavKbd);

	function handleNavKeyDown(event: ReactKeyboardEvent<HTMLElement>) {
		if (event.defaultPrevented || event.nativeEvent.isComposing || event.keyCode === 229) return;
		const links = [...(navRef.current?.querySelectorAll<HTMLElement>(".settings-nav-link") ?? [])];
		if (!links.length) return;
		const currentIdx = Math.max(0, links.indexOf(document.activeElement as HTMLElement));
		if (event.key === "ArrowDown" && !event.altKey && !event.ctrlKey && !event.metaKey && !event.shiftKey) {
			const first =
				routeRef.current?.querySelector<HTMLElement>(
					'[tabindex="0"]:not(:disabled), button:not(:disabled):not([tabindex="-1"]), a[href]:not([tabindex="-1"]), input:not(:disabled):not([tabindex="-1"])',
				) ?? routeRef.current?.querySelector<HTMLElement>(".settings-shortcuts-pane");
			if (first) {
				event.preventDefault();
				setNavKbdActive(false);
				first.focus();
			}
			return;
		}
		const nextIdx = resolveToolbarNavKey(
			{
				key: event.key,
				defaultPrevented: event.defaultPrevented,
				isComposing: event.nativeEvent.isComposing,
				keyCode: event.keyCode,
				ctrlKey: event.ctrlKey,
				metaKey: event.metaKey,
				altKey: event.altKey,
				shiftKey: event.shiftKey,
			},
			currentIdx,
			links.length,
		);
		if (nextIdx !== null) {
			event.preventDefault();
			setNavKbdActive(true);
			const targetTab = tabs[nextIdx];
			if (targetTab) {
				links[nextIdx]?.focus();
				if (targetTab.to !== activePath) {
					void navigate(targetTab.to, { viewTransition: true });
				}
			}
		}
	}

	useEffect(() => {
		const pagesEl = pagesRef.current;
		if (!pagesEl) return;
		function onRouteKeyDown(event: KeyboardEvent) {
			const routeEl = routeRef.current;
			if (
				!routeEl ||
				event.defaultPrevented ||
				event.isComposing ||
				event.keyCode === 229 ||
				(event.key !== "ArrowUp" && event.key !== "ArrowDown") ||
				event.altKey ||
				event.ctrlKey ||
				event.metaKey ||
				event.shiftKey
			) {
				return;
			}
			const active = document.activeElement as HTMLElement | null;
			if (!active || !routeEl.contains(active)) return;
			if (active.closest('[role="dialog"], [data-slot="popover"]')) return;
			if (active.tagName === "TEXTAREA" || active.tagName === "SELECT") return;
			if (active instanceof HTMLInputElement) {
				if (event.key === "ArrowUp") {
					if ((active.selectionStart ?? 0) !== 0 || (active.selectionEnd ?? 0) !== 0) return;
				} else {
					const end = active.value.length;
					if ((active.selectionStart ?? 0) !== end || (active.selectionEnd ?? 0) !== end) return;
				}
			}
			const isShortcutsPane = active.classList.contains("settings-shortcuts-pane");
			if (isShortcutsPane) {
				if (event.key === "ArrowDown") return;
				const scroller = active.closest<HTMLElement>(".settings-scroll");
				if (scroller && scroller.scrollTop > 0) return;
				const currentTab = navRef.current?.querySelector<HTMLElement>(
					'.settings-nav-link.is-current, .settings-nav-link[tabindex="0"]',
				);
				if (currentTab) {
					event.preventDefault();
					setNavKbdActive(true);
					currentTab.focus();
				}
				return;
			}
			const focusables = [
				...routeEl.querySelectorAll<HTMLElement>(
					'[tabindex="0"]:not(:disabled), button:not(:disabled):not([tabindex="-1"]), a[href]:not([tabindex="-1"]), input:not(:disabled):not([tabindex="-1"])',
				),
			];
			const idx = focusables.indexOf(active);
			if (idx < 0) return;
			if (event.key === "ArrowUp") {
				if (idx === 0) {
					const currentTab = navRef.current?.querySelector<HTMLElement>(
						'.settings-nav-link.is-current, .settings-nav-link[tabindex="0"]',
					);
					if (currentTab) {
						event.preventDefault();
						setNavKbdActive(true);
						currentTab.focus();
					}
				} else {
					event.preventDefault();
					focusables[idx - 1]?.focus();
				}
			} else if (idx + 1 < focusables.length) {
				event.preventDefault();
				focusables[idx + 1]?.focus();
			}
		}
		pagesEl.addEventListener("keydown", onRouteKeyDown);
		return () => pagesEl.removeEventListener("keydown", onRouteKeyDown);
	}, []);

	useEffect(() => {
		void preloadSettings();
		return () => {
			// TUI 可能在 Web 设置页关闭期间更改共享文件；下次进入重新读取。
			for (const path of settingsPaths) invalidateSetting(path);
		};
	}, []);
	useGlobalEvents((name, raw) => {
		let paths: readonly string[] = settingsPaths;
		if (name === "state") {
			try {
				const type = JSON.parse(raw?.data ?? "null")?.type;
				if (type === "models_changed") paths = ["/api/models"];
				else if (type !== "auth_changed" && type !== "settings_changed") return;
			} catch {
				return;
			}
		} else if (name !== "open" && name !== "reset") return;
		for (const path of paths) invalidateSetting(path);
		void preloadSettings(paths);
	});
	return (
		<main
			className="chat-main settings-main settings-workspace"
			data-settings-slide={
				navigation.direction === 0 ? undefined : navigation.direction > 0 ? "forward" : "backward"
			}
		>
			<header className="chat-header settings-header">
				<SidebarHeaderToggle />
				<SettingsIcon size={16} aria-hidden="true" />
				<h1>设置</h1>
			</header>
			<SettingsFeedback>
				<div className="settings-scroll">
					<div className="settings-content settings-pages" ref={pagesRef}>
						<nav
							ref={navRef}
							className="settings-nav"
							aria-label="设置分类"
							data-kbd-active={navKbdActive ? "true" : undefined}
							onKeyDown={handleNavKeyDown}
							onBlur={(event) => {
								if (!event.currentTarget.contains(event.relatedTarget as Node | null)) {
									setNavKbdActive(false);
								}
							}}
						>
							{tabs.map((tab, tabIdx) => {
								const isCurrent = activePath === tab.to;
								return (
									<SettingsLink
										key={tab.to}
										to={tab.to}
										tabIndex={tabIdx === tabStopIdx ? 0 : -1}
										aria-current={isCurrent ? "page" : undefined}
										className={`settings-nav-link${isCurrent ? " is-current" : ""}`}
										prefetch="render"
										onFocus={(event) => {
											if (event.currentTarget.matches(":focus-visible")) setNavKbdActive(true);
										}}
										onClick={(event) => {
											if (event.detail > 0) setNavKbdActive(false);
										}}
									>
										{tab.label}
										{isCurrent && <span className="settings-nav-indicator" aria-hidden="true" />}
									</SettingsLink>
								);
							})}
						</nav>
						<div className="settings-route" key={pathname} ref={routeRef}>
							<Outlet />
						</div>
					</div>
				</div>
			</SettingsFeedback>
		</main>
	);
}
