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

/** 共用页面监听；有锚点时只响应会移动锚点的滚动，Esc 关闭且焦点落空时归还触发器焦点。 */
export function usePanelDismiss(
	open: boolean,
	onDismiss: () => void,
	anchor?: RefObject<HTMLElement | null>,
	returnFocus?: RefObject<HTMLElement | null>,
) {
	const callback = useRef(onDismiss);
	callback.current = onDismiss;
	const escaped = useRef(false);
	const focusTarget = returnFocus ?? anchor;
	useEffect(() => {
		if (!open) {
			if (escaped.current) {
				escaped.current = false;
				const restore = () => {
					if (document.activeElement === document.body) {
						focusTarget?.current?.focus({ preventScroll: true });
					}
				};
				if (focusTarget?.current?.closest("[inert]")) requestAnimationFrame(restore);
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
	}, [open, anchor, focusTarget]);
}
