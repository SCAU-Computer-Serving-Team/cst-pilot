import {
	type KeyboardEvent as ReactKeyboardEvent,
	type RefObject,
	useCallback,
	useEffect,
	useLayoutEffect,
	useRef,
	useState,
} from "react";
import { apiJson } from "../data/api";
import { subscribeGlobalEvents } from "../data/event-stream";
import { type PointerPosition, pointerMoved, resolveDialogActionKey } from "./keyboard-navigation";

export type KeyboardFocusStyle = "brackets" | "rail";

const STYLE_EVENT = "cst:kbd-focus-style";

export function normalizeKeyboardFocusStyle(raw: unknown): KeyboardFocusStyle {
	return raw === "rail" ? "rail" : "brackets";
}

function currentKeyboardFocusStyle(): KeyboardFocusStyle {
	if (typeof document === "undefined") return "brackets";
	return normalizeKeyboardFocusStyle(document.documentElement.dataset.kbdFocus);
}

export function applyKeyboardFocusStyle(style: KeyboardFocusStyle): void {
	document.documentElement.dataset.kbdFocus = style;
	window.dispatchEvent(new CustomEvent<KeyboardFocusStyle>(STYLE_EVENT, { detail: style }));
}

/** 读取并订阅 Web 方向键选中样式。 */
export function useKeyboardFocusStyle(): KeyboardFocusStyle {
	const [style, setStyle] = useState<KeyboardFocusStyle>(currentKeyboardFocusStyle);
	useEffect(() => {
		let controller: AbortController | undefined;
		const refresh = () => {
			controller?.abort();
			const request = new AbortController();
			controller = request;
			void apiJson<{ keyboardFocusStyle?: string }>("/api/settings", { signal: request.signal })
				.then((data) => {
					if (request.signal.aborted) return;
					const next = normalizeKeyboardFocusStyle(data.keyboardFocusStyle);
					document.documentElement.dataset.kbdFocus = next;
					setStyle(next);
				})
				.catch(() => {
					/* 保留上次确认的样式。 */
				});
		};
		const onLocal = () => {
			controller?.abort();
			setStyle(currentKeyboardFocusStyle());
		};
		window.addEventListener(STYLE_EVENT, onLocal);
		refresh();
		const unsubscribe = subscribeGlobalEvents((name, event) => {
			if (name === "open" || name === "reset") {
				refresh();
				return;
			}
			if (name !== "state") return;
			try {
				const change = JSON.parse(event?.data ?? "null");
				if (change?.type === "settings_changed" && change.scope === "web") refresh();
			} catch {
				/* 忽略格式异常事件。 */
			}
		});
		return () => {
			window.removeEventListener(STYLE_EVENT, onLocal);
			unsubscribe();
			controller?.abort();
		};
	}, []);
	return style;
}

/** 键盘接管时记录鼠标位置。后续移动以该点为基准，累计判定 4px 阈值。 */
export function usePointerMutex(active: boolean, onClear: () => void) {
	const last = useRef<PointerPosition | null>(null);
	const origin = useRef<PointerPosition | null>(null);
	useLayoutEffect(() => {
		origin.current = active ? last.current : null;
	}, [active]);
	useEffect(() => {
		const onMove = (event: MouseEvent) => {
			const point = { x: event.clientX, y: event.clientY };
			last.current = point;
			if (!active) return;
			origin.current ??= point;
			if (pointerMoved(origin.current, point)) {
				origin.current = point;
				onClear();
			}
		};
		window.addEventListener("mousemove", onMove, { passive: true });
		return () => window.removeEventListener("mousemove", onMove);
	}, [active, onClear]);
}

export type KeyboardFocusTone = "neutral" | "accent" | "danger";
export type KeyboardRailPlacement = "inside" | "outside" | "bottom";

interface HaloRect {
	x: number;
	y: number;
	width: number;
	height: number;
	borderRadius?: string;
}

export function KbdFocusOverlay({
	containerRef,
	itemRefs,
	targetId,
	as: Element = "div",
	style,
	tone = "neutral",
	railPlacement = "inside",
	bracketOutset = 0,
	insetX = 0,
	insetY = 0,
}: {
	containerRef: RefObject<HTMLElement | null>;
	itemRefs: RefObject<ReadonlyMap<string, HTMLElement>>;
	targetId: string | null;
	as?: "div" | "li";
	style: KeyboardFocusStyle;
	tone?: KeyboardFocusTone;
	railPlacement?: KeyboardRailPlacement;
	bracketOutset?: number;
	insetX?: number;
	insetY?: number;
}) {
	const [geometry, setGeometry] = useState<{ rect: HaloRect; instant: boolean } | null>(null);

	const wasVisible = useRef(false);
	const measure = useCallback(
		(instant: boolean) => {
			if (targetId === null) return;
			const container = containerRef.current;
			const row = itemRefs.current.get(targetId);
			if (!container || !row) return;
			const cRect = container.getBoundingClientRect();
			const tRect = row.getBoundingClientRect();
			const x = tRect.left - cRect.left - container.clientLeft + container.scrollLeft;
			const y = tRect.top - cRect.top - container.clientTop + container.scrollTop;
			const w = tRect.width;
			const h = tRect.height;
			let rect: HaloRect;
			if (style === "rail") {
				if (railPlacement === "bottom") {
					const barW = Math.max(16, Math.min(34, w * 0.36));
					rect = { x: x + (w - barW) / 2, y: y + h + 4, width: barW, height: 3, borderRadius: "9999px" };
				} else if (railPlacement === "outside") {
					const railH = Math.max(14, Math.min(22, h * 0.56));
					rect = { x: x - 7, y: y + (h - railH) / 2, width: 3, height: railH, borderRadius: "9999px" };
				} else {
					const railH = Math.max(16, Math.min(26, h * 0.56));
					rect = {
						x: Math.max(2, x + insetX + 3),
						y: y + (h - railH) / 2,
						width: 3,
						height: railH,
						borderRadius: "9999px",
					};
				}
			} else {
				const baseRadius =
					bracketOutset > 0 ? Number.parseFloat(getComputedStyle(row).borderRadius) || 4 : undefined;
				rect = {
					x: x + insetX - bracketOutset,
					y: y + insetY - bracketOutset,
					width: Math.max(12, w - insetX * 2 + bracketOutset * 2),
					height: Math.max(12, h - insetY * 2 + bracketOutset * 2),
					borderRadius: baseRadius !== undefined ? `${baseRadius + bracketOutset}px` : undefined,
				};
			}
			setGeometry((previous) => {
				const old = previous?.rect;
				if (
					old &&
					old.x === rect.x &&
					old.y === rect.y &&
					old.width === rect.width &&
					old.height === rect.height &&
					old.borderRadius === rect.borderRadius
				)
					return previous;
				return { rect, instant };
			});
		},
		[containerRef, itemRefs, targetId, style, railPlacement, bracketOutset, insetX, insetY],
	);

	// 插入、筛选和排序会移动行，即使它们的尺寸没有变化。
	useLayoutEffect(() => {
		measure(!wasVisible.current);
		wasVisible.current = targetId !== null;
	});
	useEffect(() => {
		if (targetId === null) return;
		const container = containerRef.current;
		const row = itemRefs.current.get(targetId);
		if (!container || !row) return;
		const observer = new ResizeObserver(() => measure(true));
		observer.observe(container);
		observer.observe(row);
		return () => observer.disconnect();
	}, [containerRef, itemRefs, targetId, measure]);

	if (!geometry) return null;
	const { rect, instant } = geometry;
	return (
		<Element
			className="kbd-focus-halo"
			aria-hidden="true"
			data-style={style}
			data-tone={tone}
			data-visible={targetId !== null ? "true" : "false"}
			data-instant={instant ? "true" : "false"}
			style={{
				transform: `translate3d(${rect.x}px, ${rect.y}px, 0)`,
				width: `${rect.width}px`,
				height: `${rect.height}px`,
				borderRadius: rect.borderRadius,
			}}
		/>
	);
}

export interface DialogActionItem {
	id: string;
	tone: KeyboardFocusTone;
}

/** 模态弹窗按钮栏的横向方向键切换、边界夹紧与 180ms 滑移指示器协调 Hook。 */
export function useDialogActionNav(
	actions: readonly DialogActionItem[],
	options: {
		initialId?: string;
		requireButtonFocus?: boolean;
		disabled?: boolean;
	} = {},
) {
	const { initialId, requireButtonFocus = false, disabled = false } = options;
	const kbdStyle = useKeyboardFocusStyle();
	const containerRef = useRef<HTMLDivElement>(null);
	const itemRefs = useRef<Map<string, HTMLElement>>(new Map());
	const [kbdActive, setKbdActive] = useState(!requireButtonFocus);
	const [activeId, setActiveId] = useState<string | null>(
		requireButtonFocus ? null : (initialId ?? actions[0]?.id ?? null),
	);
	const clearKbd = useCallback(() => setKbdActive(false), []);
	usePointerMutex(kbdActive && !disabled, clearKbd);

	const registerButton = useCallback(
		(id: string, externalRef?: RefObject<HTMLButtonElement | null>) => (node: HTMLButtonElement | null) => {
			if (node) itemRefs.current.set(id, node);
			else itemRefs.current.delete(id);
			if (externalRef) externalRef.current = node;
		},
		[],
	);

	const onButtonFocus = useCallback((id: string) => {
		setActiveId(id);
	}, []);

	const onButtonPointerDown = useCallback(() => {
		setKbdActive(false);
	}, []);

	useEffect(() => {
		const container = containerRef.current;
		if (!container) return;
		const onFocusOut = (event: FocusEvent) => {
			const next = event.relatedTarget;
			if (next instanceof Node && container.contains(next)) return;
			setActiveId(null);
		};
		container.addEventListener("focusout", onFocusOut);
		return () => container.removeEventListener("focusout", onFocusOut);
	}, []);

	const onDialogKeyDown = useCallback(
		(event: ReactKeyboardEvent<HTMLElement>) => {
			if (disabled) return;
			const target = event.target;
			const isTextField = target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement;
			const isTab =
				event.key === "Tab" &&
				!event.ctrlKey &&
				!event.altKey &&
				!event.metaKey &&
				!event.nativeEvent.isComposing &&
				event.keyCode !== 229;
			if (isTab) {
				setKbdActive(true);
				return;
			}
			if (isTextField) return;
			const activeEl = document.activeElement;
			const focusedIdx = actions.findIndex((item) => itemRefs.current.get(item.id) === activeEl);
			if (requireButtonFocus && focusedIdx < 0) return;
			const currentIdx =
				focusedIdx >= 0
					? focusedIdx
					: Math.max(
							0,
							actions.findIndex((item) => item.id === activeId),
						);
			const nextIdx = resolveDialogActionKey(
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
				actions.length,
				(index) => {
					const button = itemRefs.current.get(actions[index]?.id ?? "");
					return !button || (button instanceof HTMLButtonElement && button.disabled);
				},
			);
			if (nextIdx === null) return;
			event.preventDefault();
			const nextItem = actions[nextIdx];
			if (!nextItem) return;
			setKbdActive(true);
			setActiveId(nextItem.id);
			itemRefs.current.get(nextItem.id)?.focus({ preventScroll: true });
		},
		[actions, activeId, disabled, requireButtonFocus],
	);

	const resolvedTargetId =
		!disabled && kbdActive && activeId && actions.some((item) => item.id === activeId) ? activeId : null;
	const activeTone: KeyboardFocusTone = actions.find((item) => item.id === resolvedTargetId)?.tone ?? "neutral";

	return {
		containerRef,
		itemRefs,
		kbdStyle,
		kbdActive: resolvedTargetId !== null,
		targetId: resolvedTargetId,
		activeTone,
		registerButton,
		onButtonFocus,
		onButtonPointerDown,
		onDialogKeyDown,
	};
}
