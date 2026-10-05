import { useEffect, useRef } from "react";

const overlays = ".composer-popover, .sidebar-account-popover, .login-provider-popover, .context-panel";
const dismissers = new Set<() => void>();

function pageScroll(event: Event) {
	if (event.target instanceof Element && event.target.closest(overlays)) return;
	for (const dismiss of dismissers) dismiss();
}
function escapeKey(event: KeyboardEvent) {
	if (event.key !== "Escape" || event.defaultPrevented || event.isComposing) return;
	for (const dismiss of dismissers) dismiss();
}

/** 共用一组页面监听；面板内部滚动保留打开状态。 */
export function usePanelDismiss(open: boolean, onDismiss: () => void) {
	const callback = useRef(onDismiss);
	callback.current = onDismiss;
	useEffect(() => {
		if (!open) return;
		const dismiss = () => callback.current();
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
	}, [open]);
}
