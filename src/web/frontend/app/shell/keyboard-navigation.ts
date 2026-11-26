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

type DialogKeyEvent = KeyEvent & Pick<KeyboardEvent, "key">;

/**
 * 解析确认类模态弹窗按钮栏的横向方向键。
 * 左右到达边界时夹紧停驻，避免在安全取消钮上按左键折返到危险确认钮；自动跳过禁用按钮。
 */
export function resolveDialogActionKey(
	event: DialogKeyEvent,
	currentIndex: number,
	count: number,
	isDisabled: (index: number) => boolean = () => false,
): number | null {
	if (count <= 0 || !isNavigationKeyEvent(event)) return null;
	const firstEnabled = () => {
		for (let i = 0; i < count; i++) {
			if (!isDisabled(i)) return i;
		}
		return null;
	};
	const lastEnabled = () => {
		for (let i = count - 1; i >= 0; i--) {
			if (!isDisabled(i)) return i;
		}
		return null;
	};
	const cur = Math.max(0, Math.min(currentIndex, count - 1));
	switch (event.key) {
		case "ArrowLeft": {
			for (let i = cur - 1; i >= 0; i--) {
				if (!isDisabled(i)) return i;
			}
			return !isDisabled(cur) ? cur : firstEnabled();
		}
		case "ArrowRight": {
			for (let i = cur + 1; i < count; i++) {
				if (!isDisabled(i)) return i;
			}
			return !isDisabled(cur) ? cur : lastEnabled();
		}
		case "Home":
			return firstEnabled();
		case "End":
			return lastEnabled();
		default:
			return null;
	}
}

/**
 * 解析纵向菜单或列表的方向键（`↑` / `↓` / `Home` / `End`），自动跳过禁用项。
 */
export function resolveListNavKey(
	event: DialogKeyEvent,
	currentIndex: number,
	count: number,
	options: { wrap?: boolean; isDisabled?: (index: number) => boolean } = {},
): number | null {
	if (count <= 0 || !isNavigationKeyEvent(event)) return null;
	const { wrap = false, isDisabled = () => false } = options;
	const firstEnabled = () => {
		for (let i = 0; i < count; i++) {
			if (!isDisabled(i)) return i;
		}
		return null;
	};
	const lastEnabled = () => {
		for (let i = count - 1; i >= 0; i--) {
			if (!isDisabled(i)) return i;
		}
		return null;
	};
	const cur = Math.max(0, Math.min(currentIndex, count - 1));
	switch (event.key) {
		case "ArrowUp": {
			for (let step = 1; step <= count; step++) {
				const raw = cur - step;
				if (raw < 0 && !wrap) return !isDisabled(cur) ? cur : firstEnabled();
				const idx = (raw + count) % count;
				if (!isDisabled(idx)) return idx;
			}
			return null;
		}
		case "ArrowDown": {
			for (let step = 1; step <= count; step++) {
				const raw = cur + step;
				if (raw >= count && !wrap) return !isDisabled(cur) ? cur : lastEnabled();
				const idx = raw % count;
				if (!isDisabled(idx)) return idx;
			}
			return null;
		}
		case "Home":
			return firstEnabled();
		case "End":
			return lastEnabled();
		default:
			return null;
	}
}

/**
 * 解析横向工具栏或选项卡的方向键（`←` / `→` / `Home` / `End`），支持循环或边界夹紧并跳过禁用项。
 */
export function resolveToolbarNavKey(
	event: DialogKeyEvent,
	currentIndex: number,
	count: number,
	options: { wrap?: boolean; isDisabled?: (index: number) => boolean } = {},
): number | null {
	if (count <= 0 || !isNavigationKeyEvent(event)) return null;
	const { wrap = false, isDisabled = () => false } = options;
	if (!wrap) return resolveDialogActionKey(event, currentIndex, count, isDisabled);
	const firstEnabled = () => {
		for (let i = 0; i < count; i++) {
			if (!isDisabled(i)) return i;
		}
		return null;
	};
	const lastEnabled = () => {
		for (let i = count - 1; i >= 0; i--) {
			if (!isDisabled(i)) return i;
		}
		return null;
	};
	const cur = Math.max(0, Math.min(currentIndex, count - 1));
	switch (event.key) {
		case "ArrowLeft": {
			for (let step = 1; step <= count; step++) {
				const idx = (cur - step + count) % count;
				if (!isDisabled(idx)) return idx;
			}
			return null;
		}
		case "ArrowRight": {
			for (let step = 1; step <= count; step++) {
				const idx = (cur + step) % count;
				if (!isDisabled(idx)) return idx;
			}
			return null;
		}
		case "Home":
			return firstEnabled();
		case "End":
			return lastEnabled();
		default:
			return null;
	}
}

/** Scroll only the list, without moving the enclosing layout or stealing focus. */
export function scrollRowIntoView(list: HTMLElement, row: HTMLElement): void {
	const top = list.getBoundingClientRect().top + list.clientTop;
	const bounds = row.getBoundingClientRect();
	const bottom = top + list.clientHeight;
	if (bounds.top < top) list.scrollTop += bounds.top - top;
	else if (bounds.bottom > bottom) list.scrollTop += bounds.bottom - bottom;
}
