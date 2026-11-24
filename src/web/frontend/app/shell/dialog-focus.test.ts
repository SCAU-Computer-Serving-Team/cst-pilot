import assert from "node:assert/strict";
import { test } from "node:test";
import { focusableElements, isTabbableElement, resolveDialogTabTarget } from "./dialog-focus.ts";

test("resolveDialogTabTarget 在弹窗首尾循环且阻止外部焦点逃逸", () => {
	assert.equal(resolveDialogTabTarget([], null, false), "dialog");
	assert.equal(resolveDialogTabTarget([], null, true), "dialog");

	const items = ["cancel", "input", "confirm"] as const;
	// 焦点已逃逸至弹窗外（activeInside === null）时拉回首项或末项
	assert.equal(resolveDialogTabTarget(items, null, false), "cancel");
	assert.equal(resolveDialogTabTarget(items, null, true), "confirm");

	// 中间与非边界推进放行浏览器原生顺序
	assert.equal(resolveDialogTabTarget(items, "cancel", false), null);
	assert.equal(resolveDialogTabTarget(items, "input", false), null);
	assert.equal(resolveDialogTabTarget(items, "input", true), null);
	assert.equal(resolveDialogTabTarget(items, "confirm", true), null);

	// 末尾 Tab 回首项，首项 Shift+Tab 回末项
	assert.equal(resolveDialogTabTarget(items, "confirm", false), "cancel");
	assert.equal(resolveDialogTabTarget(items, "cancel", true), "confirm");
});

test("focusableElements 排除 tabindex 小于 0 与隐藏元素", () => {
	assert.equal(isTabbableElement({ tabIndex: 0, hidden: false }), true);
	assert.equal(isTabbableElement({ tabIndex: -1, hidden: false }), false);
	assert.equal(isTabbableElement({ tabIndex: -2, hidden: false }), false);
	assert.equal(isTabbableElement({ tabIndex: 0, hidden: true }), false);

	const makeNode = (id: string, tabIndex: number, hidden = false, display = "block") =>
		({
			id,
			tabIndex,
			hidden,
			display,
			closest: () => null,
		}) as unknown as HTMLElement & { display: string };

	const input = makeNode("input", 0);
	const rowBtn = makeNode("row-btn", -1);
	const hiddenBtn = makeNode("hidden-btn", 0, true);
	const noneBtn = makeNode("none-btn", 0, false, "none");
	const confirmBtn = makeNode("confirm-btn", 0);
	const fakeRoot = {
		querySelectorAll: () => [input, rowBtn, hiddenBtn, noneBtn, confirmBtn],
	} as unknown as ParentNode;

	const prevGetComputedStyle = globalThis.getComputedStyle;
	globalThis.getComputedStyle = ((el: Element) => ({
		display: (el as unknown as { display: string }).display,
		visibility: "visible",
	})) as typeof getComputedStyle;
	try {
		const result = focusableElements(fakeRoot);
		assert.deepEqual(
			result.map((el) => el.id),
			["input", "confirm-btn"],
		);
	} finally {
		globalThis.getComputedStyle = prevGetComputedStyle;
	}
});
