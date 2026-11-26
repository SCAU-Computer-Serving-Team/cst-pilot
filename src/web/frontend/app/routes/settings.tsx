import "../settings/settings.css";
import { Settings as SettingsIcon } from "lucide-react";
import { type KeyboardEvent as ReactKeyboardEvent, useCallback, useEffect, useRef, useState } from "react";
import { Outlet, useLocation, useNavigate } from "react-router";
import { useGlobalEvents } from "../data/web-state";
import { SettingsFeedback, SettingsLink } from "../settings/settings-frame";
import { invalidateSetting, preloadSettings, settingsPaths } from "../settings/settings-resource-cache";
import { KbdFocusOverlay, useKeyboardFocusStyle, usePointerMutex } from "../shell/kbd-focus";
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
	const activePath = ["/settings/add", "/settings/provider", "/settings/custom"].includes(pathname)
		? "/settings"
		: pathname;
	const index = tabs.findIndex((tab) => tab.to === activePath);
	const [navigation, setNavigation] = useState({ pathname, index, direction: 0 });
	if (navigation.pathname !== pathname) {
		setNavigation({
			pathname,
			index,
			direction: index < 0 || navigation.index < 0 ? 0 : Math.sign(index - navigation.index),
		});
	}
	const kbdStyle = useKeyboardFocusStyle();
	const navRef = useRef<HTMLElement>(null);
	const navItemRefs = useRef<Map<string, HTMLElement>>(new Map());
	const routeRef = useRef<HTMLDivElement>(null);
	const [focusedTabId, setFocusedTabId] = useState<string | null>(null);
	const [navKbdActive, setNavKbdActive] = useState(false);
	const clearNavKbd = useCallback(() => setNavKbdActive(false), []);
	usePointerMutex(navKbdActive, clearNavKbd);

	function handleNavKeyDown(event: ReactKeyboardEvent<HTMLElement>) {
		if (event.defaultPrevented || event.nativeEvent.isComposing || event.keyCode === 229) return;
		const links = [...(navRef.current?.querySelectorAll<HTMLElement>(".settings-nav-link") ?? [])];
		if (!links.length) return;
		const currentIdx = Math.max(0, links.indexOf(document.activeElement as HTMLElement));
		if (event.key === "ArrowDown" && !event.altKey && !event.ctrlKey && !event.metaKey && !event.shiftKey) {
			const first = routeRef.current?.querySelector<HTMLElement>(
				'[tabindex="0"]:not(:disabled), button:not(:disabled):not([tabindex="-1"]), a[href]:not([tabindex="-1"]), input:not(:disabled):not([tabindex="-1"])',
			);
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
				setFocusedTabId(targetTab.to);
				links[nextIdx]?.focus();
				if (targetTab.to !== activePath) {
					void navigate(targetTab.to, { viewTransition: true });
				}
			}
		}
	}

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
					<div className="settings-content settings-pages">
						<nav
							ref={navRef}
							className="settings-nav"
							aria-label="设置分类"
							data-kbd-active={navKbdActive && focusedTabId !== null ? "true" : undefined}
							onKeyDown={handleNavKeyDown}
							onBlur={(event) => {
								if (!event.currentTarget.contains(event.relatedTarget as Node | null)) {
									setFocusedTabId(null);
									setNavKbdActive(false);
								}
							}}
						>
							{tabs.map((tab) => (
								<SettingsLink
									key={tab.to}
									to={tab.to}
									ref={(node) => {
										if (node) navItemRefs.current.set(tab.to, node);
										else navItemRefs.current.delete(tab.to);
									}}
									aria-current={activePath === tab.to ? "page" : undefined}
									className={`settings-nav-link${activePath === tab.to ? " is-current" : ""}`}
									prefetch="render"
									onFocus={(event) => {
										setFocusedTabId(tab.to);
										if (event.currentTarget.matches(":focus-visible")) setNavKbdActive(true);
									}}
									onClick={() => setNavKbdActive(false)}
								>
									{tab.label}
									{activePath === tab.to && <span className="settings-nav-indicator" aria-hidden="true" />}
								</SettingsLink>
							))}
							<KbdFocusOverlay
								containerRef={navRef}
								itemRefs={navItemRefs}
								targetId={navKbdActive ? focusedTabId : null}
								style={kbdStyle}
								railPlacement="bottom"
								bracketOutset={3}
							/>
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
