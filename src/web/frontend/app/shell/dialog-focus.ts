import { type RefObject, useEffect } from "react";

const FOCUSABLE_SELECTOR =
	'a[href], button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex]:not([tabindex="-1"])';

export function focusableElements(root: ParentNode): HTMLElement[] {
	return [...root.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)].filter((element) => {
		if (element.hidden || element.closest("[hidden], [inert], [aria-hidden='true']")) return false;
		const style = getComputedStyle(element);
		return style.display !== "none" && style.visibility !== "hidden";
	});
}

/** 计算模态弹窗内按 Tab / Shift+Tab 时的拦截目标；返回 null 表示放行浏览器原生顺序。 */
export function resolveDialogTabTarget<T>(
	items: readonly T[],
	activeInside: T | null,
	shiftKey: boolean,
): T | "dialog" | null {
	const first = items[0];
	const last = items.at(-1);
	if (!first || !last) return "dialog";
	if (shiftKey) {
		return !activeInside || activeInside === first ? last : null;
	}
	return !activeInside || activeInside === last ? first : null;
}

/** 将 Tab / Shift+Tab 严格循环限制在原生模态 <dialog> 内部，防止焦点逃逸到 document.body。 */
export function trapDialogTab(event: KeyboardEvent, dialog: HTMLDialogElement | null): boolean {
	if (event.key !== "Tab" || event.isComposing || !dialog || !dialog.open) return false;
	const items = focusableElements(dialog);
	const active =
		document.activeElement instanceof HTMLElement && dialog.contains(document.activeElement)
			? document.activeElement
			: null;
	const target = resolveDialogTabTarget(items, active, event.shiftKey);
	if (!target) return false;
	event.preventDefault();
	if (target === "dialog") dialog.focus();
	else target.focus();
	return true;
}

/**
 * 窗口级捕获监听确保即便焦点短暂落在 document.body（如相邻浮层刚卸载时），
 * 按 Tab 也会立即回到当前打开的 <dialog> 内部，并在挂载下一帧兜底锁定初始焦点。
 */
export function useDialogFocusTrap(
	dialogRef: RefObject<HTMLDialogElement | null>,
	initialFocusRef?: RefObject<HTMLElement | null>,
): void {
	useEffect(() => {
		const onKeyDown = (event: KeyboardEvent) => {
			trapDialogTab(event, dialogRef.current);
		};
		window.addEventListener("keydown", onKeyDown, true);
		const frame = requestAnimationFrame(() => {
			const node = dialogRef.current;
			if (!node?.open) return;
			const preferred = initialFocusRef?.current;
			if (preferred && !preferred.hasAttribute("disabled")) {
				if (document.activeElement !== preferred) preferred.focus({ preventScroll: true });
				return;
			}
			if (!node.contains(document.activeElement)) {
				focusableElements(node)[0]?.focus({ preventScroll: true });
			}
		});
		return () => {
			cancelAnimationFrame(frame);
			window.removeEventListener("keydown", onKeyDown, true);
		};
	}, [dialogRef, initialFocusRef]);
}
