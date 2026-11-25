import assert from "node:assert/strict";
import { test } from "node:test";
import { isNavigationKeyEvent, pointerMoved } from "./keyboard-navigation.ts";

test("按键交由已经消费事件的控件或输入法处理", () => {
	const plain = {
		defaultPrevented: false,
		isComposing: false,
		keyCode: 40,
		ctrlKey: false,
		metaKey: false,
		altKey: false,
		shiftKey: false,
	};
	assert.equal(isNavigationKeyEvent(plain), true);
	assert.equal(isNavigationKeyEvent({ ...plain, isComposing: true }), false);
	assert.equal(isNavigationKeyEvent({ ...plain, keyCode: 229 }), false);
	assert.equal(isNavigationKeyEvent({ ...plain, defaultPrevented: true }), false);
	assert.equal(isNavigationKeyEvent({ ...plain, shiftKey: true }), false);
});

test("鼠标从键盘接管点累计移动 4px 后解除选中", () => {
	const origin = { x: 20, y: 10 };
	assert.equal(pointerMoved(origin, { x: 19, y: 12 }), false);
	assert.equal(pointerMoved(origin, { x: 18, y: 12 }), true);
});
