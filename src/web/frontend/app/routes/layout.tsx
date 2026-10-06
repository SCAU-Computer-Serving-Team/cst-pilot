import { PanelLeft } from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { Outlet, useLocation, useSearchParams, useViewTransitionState } from "react-router";
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
	const homeTransition = useViewTransitionState("/");
	const [settledHome, setSettledHome] = useState(home);
	const homeExit = homeTransition && settledHome;
	useEffect(() => {
		if (!homeTransition) setSettledHome(home);
	}, [home, homeTransition]);
	const curtainLeft = useRef<HTMLCanvasElement>(null);
	const curtainRight = useRef<HTMLCanvasElement>(null);
	useLayoutEffect(() => {
		if (!homeExit || !home) return;
		const backdrop = document.querySelector<HTMLElement>(".home-backdrop");
		if (!backdrop) return;
		const rect = backdrop.getBoundingClientRect();
		for (const [target, side] of [
			[curtainLeft.current, "left"],
			[curtainRight.current, "right"],
		] as const) {
			if (!target) continue;
			const slice = target.parentElement!;
			slice.style.top = `${rect.top}px`;
			slice.style.left = `${side === "left" ? rect.left : rect.left + rect.width / 2 - 32}px`;
			slice.style.width = `${rect.width / 2 + 32}px`;
			slice.style.height = `${rect.height}px`;
			slice.style.backgroundImage = getComputedStyle(backdrop).backgroundImage;
			slice.style.backgroundSize = `${rect.width}px ${rect.height}px`;
			slice.style.backgroundPosition = side === "left" ? "0 0" : `${-rect.width / 2 + 32}px 0`;
			target.width = 1;
			target.height = 1;
		}
		window.dispatchEvent(
			new CustomEvent("cst-home-curtain-capture", {
				detail: { left: curtainLeft.current, right: curtainRight.current },
			}),
		);
	}, [homeExit, home]);
	const narrow = useMediaQuery(NARROW_QUERY);
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
	const overlay = narrow && !collapsed;
	return (
		<div
			ref={motion.root}
			data-home-transition={homeTransition || undefined}
			data-home-exit={homeExit || undefined}
			data-view-transitions={typeof document.startViewTransition === "function"}
			className={`app-layout ${home ? "home-layout" : "chat-layout"} ${collapsed ? "sidebar-collapsed" : ""}`}
		>
			{overlay && <div className="sidebar-backdrop" aria-hidden="true" onClick={() => changeCollapsed(true)} />}
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
			<div className="home-exit-curtain home-exit-curtain--left" aria-hidden="true">
				<canvas ref={curtainLeft} />
			</div>
			<div className="home-exit-curtain home-exit-curtain--right" aria-hidden="true">
				<canvas ref={curtainRight} />
			</div>
			<div className="workspace-content">
				<Outlet />
			</div>
		</div>
	);
}
