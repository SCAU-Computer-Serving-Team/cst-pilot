import { type RefObject, useEffect, useRef } from "react";

const overlays =
	".composer-popover, .sidebar-account-popover, .chat-more-popover, .login-provider-popover, .export-select-popover, .tree-filter-popover, .settings-default-popover, .settings-provider-popover, .settings-protocol-popover, .context-panel, .command-suggestions, .palette-modal";

type DismissEntry = {
	id: symbol;
	dismiss: (event: Event) => void;
};

const dismissStack: DismissEntry[] = [];

export function pushDismissEntry(entry: DismissEntry): void {
	const existing = dismissStack.findIndex((item) => item.id === entry.id);
	if (existing >= 0) dismissStack.splice(existing, 1);
	dismissStack.push(entry);
}

export function removeDismissEntry(id: symbol): void {
	const index = dismissStack.findIndex((item) => item.id === id);
	if (index >= 0) dismissStack.splice(index, 1);
}

export function isTopDismissEntry(id: symbol): boolean {
	return dismissStack.length > 0 && dismissStack.at(-1)?.id === id;
}

export function hasOpenDismissiblePanel(): boolean {
	return dismissStack.length > 0;
}

/** 在捕获阶段拦截输入法组合态的 Esc，阻止第三方浮层（如 React Aria Popover）在未取消输入法前误关，同时不调用 preventDefault 保留输入法原生取消。 */
export function guardCompositionEscape(
	event: Pick<KeyboardEvent, "key" | "isComposing" | "keyCode" | "stopPropagation" | "stopImmediatePropagation">,
): boolean {
	if (event.key === "Escape" && (event.isComposing || event.keyCode === 229) && dismissStack.length > 0) {
		event.stopPropagation();
		event.stopImmediatePropagation();
		return true;
	}
	return false;
}

export function handleEscapeDismiss(
	event: Pick<KeyboardEvent, "key" | "defaultPrevented" | "isComposing" | "keyCode" | "preventDefault">,
): boolean {
	if (
		event.key !== "Escape" ||
		event.defaultPrevented ||
		event.isComposing ||
		event.keyCode === 229 ||
		!dismissStack.length
	) {
		return false;
	}
	const top = dismissStack.at(-1);
	if (!top) return false;
	event.preventDefault();
	top.dismiss(event as unknown as Event);
	return true;
}

function pageScroll(event: Event) {
	if (event.target instanceof Element && event.target.closest(overlays)) return;
	for (const entry of [...dismissStack]) entry.dismiss(event);
}

function escapeCaptureKey(event: KeyboardEvent) {
	guardCompositionEscape(event);
}

function escapeKey(event: KeyboardEvent) {
	handleEscapeDismiss(event);
}

/**
 * 共用页面监听（LIFO 栈顺序）：
 * - 滚动时关闭所有受滚动位移影响的锚点浮层；
 * - Esc 仅关闭栈顶最上层浮层，并在焦点落空时归还触发器焦点。
 */
export function usePanelDismiss(
	open: boolean,
	onDismiss: () => void,
	anchor?: RefObject<HTMLElement | null>,
	returnFocus?: RefObject<HTMLElement | null>,
): { isTop: () => boolean } {
	const callback = useRef(onDismiss);
	callback.current = onDismiss;
	const entryIdRef = useRef<symbol>(Symbol("panel-dismiss"));
	const escaped = useRef(false);
	const focusTarget = returnFocus ?? anchor;
	useEffect(() => {
		const id = entryIdRef.current;
		if (!open) {
			if (escaped.current) {
				escaped.current = false;
				const restore = () => {
					if (document.querySelector(".standby-scrim")) return;
					if (document.activeElement === document.body) {
						focusTarget?.current?.focus({ preventScroll: true });
					}
				};
				if (focusTarget?.current?.closest("[inert]")) window.setTimeout(restore, 0);
				else restore();
			}
			return;
		}
		escaped.current = false;
		const dismiss = (event: Event) => {
			if (
				event.type === "scroll" &&
				anchor?.current &&
				event.target instanceof Element &&
				!event.target.contains(anchor.current)
			)
				return;
			if (event.type === "keydown") escaped.current = true;
			callback.current();
		};
		const wasEmpty = dismissStack.length === 0;
		pushDismissEntry({ id, dismiss });
		if (wasEmpty) {
			window.addEventListener("scroll", pageScroll, true);
			window.addEventListener("keydown", escapeCaptureKey, true);
			window.addEventListener("keydown", escapeKey);
		}
		return () => {
			removeDismissEntry(id);
			if (dismissStack.length === 0) {
				window.removeEventListener("scroll", pageScroll, true);
				window.removeEventListener("keydown", escapeCaptureKey, true);
				window.removeEventListener("keydown", escapeKey);
			}
		};
	}, [open, anchor, focusTarget]);
	return {
		isTop: () => isTopDismissEntry(entryIdRef.current),
	};
}
