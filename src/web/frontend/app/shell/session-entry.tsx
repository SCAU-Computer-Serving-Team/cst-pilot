import { Check } from "lucide-react";
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
	preview,
	index,
	viewTransition,
	onToggle,
	onNavigate,
	onOpenMenu,
}: {
	session: SessionItem;
	active: boolean;
	managing: boolean;
	selected: boolean;
	disabled: boolean;
	preview: boolean;
	index: number;
	viewTransition?: boolean;
	onToggle: () => void;
	onNavigate?: (event: MouseEvent<HTMLAnchorElement>) => void;
	onOpenMenu: (x: number, y: number) => void;
}) {
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

	function startLongPress(x: number, y: number) {
		if (disabled || preview) return;
		cancelLongPress();
		suppressClickRef.current = false;
		originRef.current = { x, y };
		timerRef.current = window.setTimeout(() => {
			timerRef.current = null;
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

	const content = (
		<>
			<span className="session-check-control" aria-hidden="true">
				<Check size={12} />
			</span>
			<span className="sidebar-selection-text">{session.title}</span>
		</>
	);
	return (
		<div className="sidebar-session-row" style={{ "--startup-row": Math.min(index + 1, 11) } as CSSProperties}>
			{managing ? (
				<label
					className="sidebar-session sidebar-session-select"
					data-session-id={session.id}
					data-selected={selected || undefined}
					data-disabled={disabled || undefined}
					title={session.title}
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
					{content}
				</label>
			) : (
				<Link
					className={`sidebar-session ${active ? "selected" : ""}`}
					data-session-id={session.id}
					aria-current={active ? "page" : undefined}
					to={`/s/${session.id}${preview ? "?preview=1" : ""}`}
					viewTransition={viewTransition}
					title={session.title}
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
					{content}
				</Link>
			)}
		</div>
	);
}
