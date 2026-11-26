import type { KeyboardEvent as ReactKeyboardEvent } from "react";

const TEXT_INPUT_EXCLUSIONS = ':not([type="hidden"]):not([type="file"]):not([type="checkbox"]):not([type="radio"])';

const PRIMARY_INPUT_SELECTORS = [
	"textarea.user-edit-input",
	".question-textarea",
	".question-input",
	'.composer-input[contenteditable="true"]',
	'.composer-input[contenteditable=""]',
	".composer-input[contenteditable]",
	".tree-search input",
	`.settings-search input${TEXT_INPUT_EXCLUSIONS}`,
	`.settings-workspace input${TEXT_INPUT_EXCLUSIONS}`,
	`.login-panel input${TEXT_INPUT_EXCLUSIONS}`,
];

const POPOVER_INPUT_CONTAINERS = [
	".settings-default-popover",
	".settings-provider-popover",
	".composer-popover",
	".tree-filter-popover",
];

const MODAL_INPUT_SELECTOR = `textarea, [contenteditable="true"], [contenteditable=""], [contenteditable], input${TEXT_INPUT_EXCLUSIONS}`;

export function isModelPickerShortcut(event: {
	ctrlKey: boolean;
	altKey: boolean;
	metaKey: boolean;
	shiftKey: boolean;
	key: string;
	code?: string;
	isComposing?: boolean;
	keyCode?: number;
}): boolean {
	if (event.isComposing || event.keyCode === 229) return false;
	if (!event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return false;
	return event.key.toLowerCase() === "m" || event.code === "KeyM";
}

export function isFocusInputShortcut(event: {
	ctrlKey: boolean;
	altKey: boolean;
	metaKey: boolean;
	shiftKey: boolean;
	key: string;
	code?: string;
	isComposing?: boolean;
	keyCode?: number;
}): boolean {
	if (event.isComposing || event.keyCode === 229) return false;
	if (!event.ctrlKey || event.altKey || event.metaKey || event.shiftKey) return false;
	return event.key.toLowerCase() === "j" || event.code === "KeyJ";
}

function isFocusableInput(el: Element | null): el is HTMLElement {
	if (!(el instanceof HTMLElement)) return false;
	if ("disabled" in el && (el as HTMLInputElement | HTMLTextAreaElement).disabled) return false;
	if (el.getAttribute("aria-disabled") === "true") return false;
	if (el.closest("[inert], [hidden], [aria-hidden='true']")) return false;
	if (el.tabIndex < 0 && !el.isContentEditable && el.tagName !== "INPUT" && el.tagName !== "TEXTAREA") return false;
	return true;
}

function firstFocusable(container: ParentNode, selector: string): HTMLElement | null {
	for (const node of container.querySelectorAll(selector)) {
		if (isFocusableInput(node)) return node;
	}
	return null;
}

/**
 * 解析当前页面或最顶层弹层中应由 `Ctrl+J` 聚焦的主输入框。
 * 若存在待机遮罩或不含输入框的阻塞弹层，返回 `null`，避免焦点穿透遮罩。
 */
export function findPrimaryInput(doc: Document): HTMLElement | null {
	if (doc.querySelector(".standby-scrim")) return null;

	const palette = doc.querySelector(".palette-scrim");
	if (palette) return firstFocusable(palette, MODAL_INPUT_SELECTOR);

	const openDialogs = doc.querySelectorAll("dialog[open]");
	const topDialog = openDialogs.length ? openDialogs[openDialogs.length - 1] : null;
	if (topDialog) return firstFocusable(topDialog, MODAL_INPUT_SELECTOR);

	const summaryScrim = doc.querySelector(".summary-scrim");
	if (summaryScrim) return firstFocusable(summaryScrim, MODAL_INPUT_SELECTOR);

	for (const selector of POPOVER_INPUT_CONTAINERS) {
		const popover = doc.querySelector(selector);
		if (popover) {
			const hit = firstFocusable(popover, MODAL_INPUT_SELECTOR);
			if (hit) return hit;
		}
	}

	const main = doc.querySelector("main") ?? doc.body;
	if (!main) return null;
	for (const selector of PRIMARY_INPUT_SELECTORS) {
		const hit = firstFocusable(main, selector);
		if (hit && !hit.closest(".sidebar")) return hit;
	}
	return null;
}

/**
 * 等效鼠标点击聚焦目标输入框：若已持焦则保留当前光标位置，否则聚焦并将富文本光标置于末尾。
 */
export function focusInputAsClick(target: HTMLElement): void {
	if (document.activeElement === target) return;
	target.focus();
	if (target.isContentEditable) {
		const range = document.createRange();
		range.selectNodeContents(target);
		range.collapse(false);
		const selection = window.getSelection();
		selection?.removeAllRanges();
		selection?.addRange(range);
	}
}

/** 安装全局 `Ctrl+J` 聚焦当前主输入框的监听（跳过导出静态渲染页与待机锁）。 */
export function installFocusInputShortcut(): () => void {
	const onKeyDown = (event: KeyboardEvent) => {
		if (!isFocusInputShortcut(event)) return;
		if (window.location.pathname === "/_export") return;
		const target = findPrimaryInput(document);
		if (!target) return;
		event.preventDefault();
		event.stopPropagation();
		if (!event.repeat) focusInputAsClick(target);
	};
	window.addEventListener("keydown", onKeyDown, true);
	return () => window.removeEventListener("keydown", onKeyDown, true);
}

export function isPlainVerticalArrow(event: {
	key: string;
	shiftKey: boolean;
	ctrlKey: boolean;
	altKey: boolean;
	metaKey: boolean;
	isComposing?: boolean;
	keyCode?: number;
}): "up" | "down" | null {
	if (event.isComposing || event.keyCode === 229) return null;
	if (event.shiftKey || event.ctrlKey || event.altKey || event.metaKey) return null;
	if (event.key === "ArrowUp") return "up";
	if (event.key === "ArrowDown") return "down";
	return null;
}

/**
 * `contentEditable` 编辑器在首行按 `↑` 或末行按 `↓` 时，若浏览器默认行为未移动光标，
 * 则自动将光标移至内容开头或末尾。
 */
export function handleContentEditableVerticalArrow(
	event: ReactKeyboardEvent<HTMLElement>,
	root: HTMLElement | null,
	getRevision: () => number,
): void {
	const direction = isPlainVerticalArrow({
		key: event.key,
		shiftKey: event.shiftKey,
		ctrlKey: event.ctrlKey,
		altKey: event.altKey,
		metaKey: event.metaKey,
		isComposing: event.nativeEvent.isComposing,
		keyCode: event.keyCode,
	});
	if (!direction || !root) return;
	const selection = window.getSelection();
	if (!selection || selection.rangeCount === 0 || !selection.isCollapsed) return;
	const beforeNode = selection.focusNode;
	const beforeOffset = selection.focusOffset;
	if (!beforeNode || !root.contains(beforeNode)) return;
	const beforeRevision = getRevision();

	window.setTimeout(() => {
		if (document.activeElement !== root || getRevision() !== beforeRevision) return;
		const after = window.getSelection();
		if (!after || !after.isCollapsed) return;
		if (after.focusNode !== beforeNode || after.focusOffset !== beforeOffset) return;
		const range = document.createRange();
		range.selectNodeContents(root);
		range.collapse(direction === "up");
		after.removeAllRanges();
		after.addRange(range);
	}, 0);
}

/**
 * 原生 `<textarea>` / `<input>` 在无上一行或下一行时按 `↑` / `↓` 跳到开头或末尾。
 */
export function handleTextFieldVerticalArrow(event: ReactKeyboardEvent<HTMLTextAreaElement | HTMLInputElement>): void {
	const direction = isPlainVerticalArrow({
		key: event.key,
		shiftKey: event.shiftKey,
		ctrlKey: event.ctrlKey,
		altKey: event.altKey,
		metaKey: event.metaKey,
		isComposing: event.nativeEvent.isComposing,
		keyCode: event.keyCode,
	});
	if (!direction) return;
	const target = event.currentTarget;
	if (target instanceof HTMLInputElement) {
		const pos = direction === "up" ? 0 : target.value.length;
		event.preventDefault();
		target.setSelectionRange(pos, pos);
		return;
	}
	const beforeStart = target.selectionStart;
	const beforeEnd = target.selectionEnd;
	if (beforeStart === null || beforeEnd === null || beforeStart !== beforeEnd) return;
	const beforeValue = target.value;
	window.setTimeout(() => {
		if (document.activeElement !== target || target.value !== beforeValue) return;
		if (target.selectionStart !== beforeStart || target.selectionEnd !== beforeEnd) return;
		const pos = direction === "up" ? 0 : target.value.length;
		target.setSelectionRange(pos, pos);
	}, 0);
}
