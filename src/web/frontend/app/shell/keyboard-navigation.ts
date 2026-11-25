export type PointerPosition = { x: number; y: number };

export function pointerMoved(origin: PointerPosition, current: PointerPosition): boolean {
	return Math.abs(current.x - origin.x) + Math.abs(current.y - origin.y) >= 4;
}

type KeyEvent = Pick<
	KeyboardEvent,
	"defaultPrevented" | "isComposing" | "keyCode" | "ctrlKey" | "metaKey" | "altKey" | "shiftKey"
>;

export function isNavigationKeyEvent(event: KeyEvent): boolean {
	return !(
		event.defaultPrevented ||
		event.isComposing ||
		event.keyCode === 229 ||
		event.ctrlKey ||
		event.metaKey ||
		event.altKey ||
		event.shiftKey
	);
}

/** Scroll only the list, without moving the enclosing layout or stealing focus. */
export function scrollRowIntoView(list: HTMLElement, row: HTMLElement): void {
	const top = list.getBoundingClientRect().top + list.clientTop;
	const bounds = row.getBoundingClientRect();
	const bottom = top + list.clientHeight;
	if (bounds.top < top) list.scrollTop += bounds.top - top;
	else if (bounds.bottom > bottom) list.scrollTop += bounds.bottom - bottom;
}
