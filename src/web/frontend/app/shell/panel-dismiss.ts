import { type RefObject, useEffect, useRef } from "react";

const overlays =
	".composer-popover, .sidebar-account-popover, .login-provider-popover, .export-select-popover, .context-panel";
const dismissers = new Set<(event: Event) => void>();

function pageScroll(event: Event) {
	if (event.target instanceof Element && event.target.closest(overlays)) return;
	for (const dismiss of dismissers) dismiss(event);
}
function escapeKey(event: KeyboardEvent) {
	if (event.key !== "Escape" || event.defaultPrevented || event.isComposing) return;
	for (const dismiss of dismissers) dismiss(event);
}

/** 共用页面监听；有锚点时只响应会移动锚点的滚动。 */
export function usePanelDismiss(open: boolean, onDismiss: () => void, anchor?: RefObject<HTMLElement | null>) {
	const callback = useRef(onDismiss);
	callback.current = onDismiss;
	useEffect(() => {
		if (!open) return;
		const dismiss = (event: Event) => {
			if (
				event.type === "scroll" &&
				anchor?.current &&
				event.target instanceof Element &&
				!event.target.contains(anchor.current)
			)
				return;
			callback.current();
		};
		dismissers.add(dismiss);
		if (dismissers.size === 1) {
			window.addEventListener("scroll", pageScroll, true);
			window.addEventListener("keydown", escapeKey);
		}
		return () => {
			dismissers.delete(dismiss);
			if (dismissers.size === 0) {
				window.removeEventListener("scroll", pageScroll, true);
				window.removeEventListener("keydown", escapeKey);
			}
		};
	}, [open, anchor]);
}
