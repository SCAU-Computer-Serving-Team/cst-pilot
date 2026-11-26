import { Check, Ellipsis } from "lucide-react";
import {
	type CSSProperties,
	type KeyboardEvent,
	type MouseEvent,
	type PointerEvent,
	type TouchEvent,
	useCallback,
	useEffect,
	useRef,
} from "react";
import { Link } from "react-router";
import type { SessionItem } from "./session-actions";

const LONG_PRESS_MS = 450;
const MOVE_TOLERANCE_PX = 10;

export function SessionEntry({
	session,
	active,
	managing,
	selected,
	disabled,
	inactive = false,
	preview,
	index,
	viewTransition,
	showMoreButton = false,
	menuOpen = false,
	onToggle,
	onNavigate,
	onOpenMenu,
}: {
	session: SessionItem;
	active: boolean;
	managing: boolean;
	selected: boolean;
	disabled: boolean;
	inactive?: boolean;
	preview: boolean;
	index: number;
	viewTransition?: boolean;
	showMoreButton?: boolean;
	menuOpen?: boolean;
	onToggle: () => void;
	onNavigate?: (event: MouseEvent<HTMLAnchorElement>) => void;
	onOpenMenu: (x: number, y: number, toggle?: boolean) => void;
}) {
	const rowRef = useRef<HTMLDivElement>(null);
	const timerRef = useRef<number | null>(null);
	const originRef = useRef<{ x: number; y: number } | null>(null);
	const suppressClickRef = useRef(false);

	const cancelLongPress = useCallback(() => {
		if (timerRef.current !== null) {
			window.clearTimeout(timerRef.current);
			timerRef.current = null;
		}
		originRef.current = null;
	}, []);

	useEffect(() => () => cancelLongPress(), [cancelLongPress]);
	// 切换批量管理、禁用、折叠隐藏或预览状态时立即取消挂起的长按，避免折叠/切态后弹出菜单。
	useEffect(() => {
		if (managing || disabled || inactive || preview) cancelLongPress();
	}, [managing, disabled, inactive, preview, cancelLongPress]);

	function startLongPress(x: number, y: number) {
		if (disabled || inactive || preview) return;
		cancelLongPress();
		suppressClickRef.current = false;
		originRef.current = { x, y };
		timerRef.current = window.setTimeout(() => {
			timerRef.current = null;
			if (!rowRef.current || rowRef.current.closest("[inert]")) return;
			suppressClickRef.current = true;
			onOpenMenu(x, y);
		}, LONG_PRESS_MS);
	}

	function checkMove(x: number, y: number) {
		if (timerRef.current === null || !originRef.current) return;
		if (Math.hypot(x - originRef.current.x, y - originRef.current.y) > MOVE_TOLERANCE_PX) {
			cancelLongPress();
		}
	}

	function handlePointerDown(event: PointerEvent<HTMLElement>) {
		if (event.pointerType === "touch") startLongPress(event.clientX, event.clientY);
	}

	function handlePointerMove(event: PointerEvent<HTMLElement>) {
		if (event.pointerType === "touch") checkMove(event.clientX, event.clientY);
	}

	function handleTouchStart(event: TouchEvent<HTMLElement>) {
		const touch = event.touches[0];
		if (touch && timerRef.current === null) startLongPress(touch.clientX, touch.clientY);
	}

	function handleTouchMove(event: TouchEvent<HTMLElement>) {
		const touch = event.touches[0];
		if (touch) checkMove(touch.clientX, touch.clientY);
	}

	function handleContextMenu(event: MouseEvent<HTMLElement>) {
		event.preventDefault();
		cancelLongPress();
		onOpenMenu(event.clientX, event.clientY);
	}

	function handleKeyDown(event: KeyboardEvent<HTMLElement>) {
		if (event.key === "ContextMenu" || (event.shiftKey && event.key === "F10")) {
			event.preventDefault();
			cancelLongPress();
			const rect = event.currentTarget.getBoundingClientRect();
			onOpenMenu(rect.left + Math.min(48, rect.width / 2), rect.bottom);
		}
	}

	const title = session.title.trim() || "新对话";
	return (
		<div
			ref={rowRef}
			className="sidebar-session-row"
			data-menu-open={menuOpen || undefined}
			data-active={(!managing && active) || undefined}
			style={{ "--startup-row": Math.min(index + 1, 11) } as CSSProperties}
		>
			{/* 持久视觉层：管理模式切换时不销毁 DOM，保证 240ms 勾选与文本滑移过渡执行 */}
			<div
				className={`sidebar-session-visual ${managing ? "sidebar-session-visual--select" : active ? "selected" : ""}`}
				data-selected={(managing && selected) || undefined}
				data-disabled={(managing && disabled) || undefined}
				aria-hidden="true"
			>
				<span className="session-check-control">
					<Check size={12} />
				</span>
				<span className="sidebar-selection-text">{title}</span>
			</div>
			{managing ? (
				<label
					className="sidebar-session sidebar-session-select sidebar-session-hit"
					data-session-id={session.id}
					data-selected={selected || undefined}
					data-disabled={disabled || undefined}
					title={title}
					onPointerDown={handlePointerDown}
					onPointerMove={handlePointerMove}
					onPointerUp={cancelLongPress}
					onPointerCancel={cancelLongPress}
					onTouchStart={handleTouchStart}
					onTouchMove={handleTouchMove}
					onTouchEnd={cancelLongPress}
					onTouchCancel={cancelLongPress}
					onContextMenu={handleContextMenu}
					onKeyDown={handleKeyDown}
					onClick={(event) => {
						if (suppressClickRef.current) {
							event.preventDefault();
							event.stopPropagation();
							suppressClickRef.current = false;
						}
					}}
				>
					<input
						className="session-native-check"
						type="checkbox"
						aria-label={title}
						checked={selected}
						disabled={disabled}
						onChange={() => {
							if (suppressClickRef.current) {
								suppressClickRef.current = false;
								return;
							}
							onToggle();
						}}
					/>
				</label>
			) : (
				<>
					<Link
						className={`sidebar-session sidebar-session-hit ${active ? "selected" : ""}`}
						data-session-id={session.id}
						aria-current={active ? "page" : undefined}
						to={`/s/${session.id}${preview ? "?preview=1" : ""}`}
						viewTransition={viewTransition}
						title={title}
						onPointerDown={handlePointerDown}
						onPointerMove={handlePointerMove}
						onPointerUp={cancelLongPress}
						onPointerCancel={cancelLongPress}
						onTouchStart={handleTouchStart}
						onTouchMove={handleTouchMove}
						onTouchEnd={cancelLongPress}
						onTouchCancel={cancelLongPress}
						onContextMenu={handleContextMenu}
						onKeyDown={handleKeyDown}
						onClick={(event) => {
							if (suppressClickRef.current) {
								event.preventDefault();
								event.stopPropagation();
								suppressClickRef.current = false;
								return;
							}
							onNavigate?.(event);
						}}
					>
						<span className="sidebar-session-sr">{title}</span>
					</Link>
					{showMoreButton && !preview && (
						<button
							type="button"
							className="btn-icon xs-24 sidebar-session-more"
							data-menu-trigger-id={session.id}
							aria-label={`会话操作：${title}`}
							aria-haspopup="menu"
							aria-expanded={menuOpen}
							onClick={(event) => {
								event.stopPropagation();
								const bounds = event.currentTarget.getBoundingClientRect();
								onOpenMenu(bounds.left, bounds.bottom + 4, true);
							}}
						>
							<Ellipsis size={14} aria-hidden="true" />
						</button>
					)}
				</>
			)}
		</div>
	);
}
