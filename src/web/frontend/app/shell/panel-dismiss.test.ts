import assert from "node:assert/strict";
import { test } from "node:test";
import {
	guardCompositionEscape,
	handleEscapeDismiss,
	hasOpenDismissiblePanel,
	isTopDismissEntry,
	pushDismissEntry,
	removeDismissEntry,
} from "./panel-dismiss.ts";

test("handleEscapeDismiss 按后进先出只关闭最上层浮层，并忽略输入法组合态", () => {
	const closed: string[] = [];
	const lower = Symbol("suggestions");
	const upper = Symbol("context-panel");
	pushDismissEntry({ id: lower, dismiss: () => closed.push("suggestions") });
	pushDismissEntry({ id: upper, dismiss: () => closed.push("context-panel") });

	try {
		assert.equal(hasOpenDismissiblePanel(), true);
		assert.equal(isTopDismissEntry(lower), false);
		assert.equal(isTopDismissEntry(upper), true);

		// 捕获阶段拦截输入法组合态 Esc，阻止第三方浮层关闭且不调用 preventDefault
		let stopped = false;
		let stoppedImmediate = false;
		assert.equal(
			guardCompositionEscape({
				key: "Escape",
				isComposing: false,
				keyCode: 229,
				stopPropagation: () => {
					stopped = true;
				},
				stopImmediatePropagation: () => {
					stoppedImmediate = true;
				},
			}),
			true,
		);
		assert.equal(stopped, true);
		assert.equal(stoppedImmediate, true);
		assert.deepEqual(closed, []);

		// IME 组合键（keyCode 229 或 isComposing）不得误关任何浮层
		let prevented = false;
		assert.equal(
			handleEscapeDismiss({
				key: "Escape",
				defaultPrevented: false,
				isComposing: false,
				keyCode: 229,
				preventDefault: () => {
					prevented = true;
				},
			}),
			false,
		);
		assert.equal(prevented, false);
		assert.deepEqual(closed, []);

		// 第一次 Esc 仅关闭栈顶 context-panel，不关闭底层 suggestions
		assert.equal(
			handleEscapeDismiss({
				key: "Escape",
				defaultPrevented: false,
				isComposing: false,
				keyCode: 27,
				preventDefault: () => {
					prevented = true;
				},
			}),
			true,
		);
		assert.equal(prevented, true);
		assert.deepEqual(closed, ["context-panel"]);

		// 栈顶卸载后，下一次 Esc 关闭次层 suggestions
		removeDismissEntry(upper);
		assert.equal(isTopDismissEntry(lower), true);
		assert.equal(
			handleEscapeDismiss({
				key: "Escape",
				defaultPrevented: false,
				isComposing: false,
				keyCode: 27,
				preventDefault: () => {},
			}),
			true,
		);
		assert.deepEqual(closed, ["context-panel", "suggestions"]);
	} finally {
		removeDismissEntry(upper);
		removeDismissEntry(lower);
		assert.equal(hasOpenDismissiblePanel(), false);
	}
});
