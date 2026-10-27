import { PanelLeft } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Outlet, useLocation, useSearchParams, useViewTransitionState } from "react-router";
import { AccountProvider } from "../account/use-account";
import { BlueHour } from "../shell/blue-hour";
import { ConnectionStatus } from "../shell/connection-status";
import { NotificationRegion } from "../shell/notification";
import { Sidebar } from "../shell/shell";
import { useSidebarMotion } from "../shell/sidebar-motion";

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

/** 工作台外壳：侧栏只挂载一次，路由切换时不再重建，避免会话列表闪空。 */
export default function WorkspaceLayout() {
	const { pathname } = useLocation();
	const [params] = useSearchParams();
	const preview = params.get("preview") === "1";
	const home = pathname === "/";
	const settings = pathname === "/settings" || pathname.startsWith("/settings/");
	const homeTransition = useViewTransitionState("/");
	const chatTransition = useViewTransitionState("/s/*");
	const accountTransition = useViewTransitionState("/account");
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
	function changeCollapsed(value: boolean) {
		motion.capture();
		setCollapsed(value);
	}
	const overlayLayout = settings ? mobile : narrow;
	const overlay = overlayLayout && !collapsed;
	return (
		<AccountProvider enabled={!preview}>
			<NotificationRegion contentInset={collapsed || (settings && mobile) ? 0 : 264}>
				<div
					ref={motion.root}
					data-home-transition={homeTransition || undefined}
					data-settings-boundary={settingsBoundary || undefined}
					data-session-transition={sessionTransition || undefined}
					data-content-transition={(routeTransition && !homeTransition && !settingsBoundary) || undefined}
					data-route-entry={entry.current.local || undefined}
					data-home-exit={homeExit || undefined}
					data-view-transitions={typeof document.startViewTransition === "function"}
					className={`app-layout ${home ? "home-layout" : "chat-layout"} ${settings ? "settings-layout" : ""} ${collapsed ? "sidebar-collapsed" : ""}`}
				>
					{overlay && (
						<div className="sidebar-backdrop" aria-hidden="true" onClick={() => changeCollapsed(true)} />
					)}
					<Sidebar
						preview={preview}
						collapsed={collapsed}
						onCollapse={() => changeCollapsed(true)}
						onNavigate={overlay ? () => changeCollapsed(true) : undefined}
					/>
					{collapsed && (
						<button
							type="button"
							className="sidebar-restore"
							aria-label="展开侧栏"
							onClick={() => changeCollapsed(false)}
						>
							<PanelLeft size={22} aria-hidden="true" />
						</button>
					)}
					<div className="workspace-content">
						{home && (
							<div className="home-backdrop" aria-hidden="true">
								<BlueHour kind="home" />
							</div>
						)}
						<ConnectionStatus enabled={!preview} />
						<Outlet />
					</div>
				</div>
			</NotificationRegion>
		</AccountProvider>
	);
}
