import assert from "node:assert/strict";
import { test } from "node:test";
import { resolveDialogTabTarget } from "./dialog-focus.ts";

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
