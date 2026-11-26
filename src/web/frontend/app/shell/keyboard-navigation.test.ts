import assert from "node:assert/strict";
import { test } from "node:test";
import {
	isNavigationKeyEvent,
	pointerMoved,
	resolveDialogActionKey,
	resolveListNavKey,
	resolveToolbarNavKey,
} from "./keyboard-navigation.ts";

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

test("确认弹窗左右方向键在边界夹紧，不从安全取消钮折返到危险确认钮", () => {
	const key = (k: string, extra: Record<string, unknown> = {}) => ({
		key: k,
		defaultPrevented: false,
		isComposing: false,
		keyCode: 0,
		ctrlKey: false,
		metaKey: false,
		altKey: false,
		shiftKey: false,
		...extra,
	});
	// 双按钮确认弹窗
	assert.equal(resolveDialogActionKey(key("ArrowLeft"), 0, 2), 0);
	assert.equal(resolveDialogActionKey(key("ArrowRight"), 0, 2), 1);
	assert.equal(resolveDialogActionKey(key("ArrowRight"), 1, 2), 1);
	assert.equal(resolveDialogActionKey(key("ArrowLeft"), 1, 2), 0);
	assert.equal(resolveDialogActionKey(key("Home"), 1, 2), 0);
	assert.equal(resolveDialogActionKey(key("End"), 0, 2), 1);

	// 三按钮缺密钥弹窗（initial 模式）
	assert.equal(resolveDialogActionKey(key("ArrowRight"), 0, 3), 1);
	assert.equal(resolveDialogActionKey(key("ArrowRight"), 1, 3), 2);
	assert.equal(resolveDialogActionKey(key("ArrowRight"), 2, 3), 2);
	assert.equal(resolveDialogActionKey(key("ArrowLeft"), 2, 3), 1);
	assert.equal(resolveDialogActionKey(key("ArrowLeft"), 1, 3), 0);
	assert.equal(resolveDialogActionKey(key("ArrowLeft"), 0, 3), 0);
	assert.equal(resolveDialogActionKey(key("End"), 0, 3), 2);
	assert.equal(resolveDialogActionKey(key("Home"), 2, 3), 0);
});

test("确认弹窗方向键自动跳过禁用按钮，并保护输入法组合态与修饰键", () => {
	const key = (k: string, extra: Record<string, unknown> = {}) => ({
		key: k,
		defaultPrevented: false,
		isComposing: false,
		keyCode: 0,
		ctrlKey: false,
		metaKey: false,
		altKey: false,
		shiftKey: false,
		...extra,
	});
	// 退出弹窗检查中：确认钮禁用时停留在取消钮
	const onlyLeftEnabled = (i: number) => i === 1;
	assert.equal(resolveDialogActionKey(key("ArrowRight"), 0, 2, onlyLeftEnabled), 0);
	assert.equal(resolveDialogActionKey(key("End"), 0, 2, onlyLeftEnabled), 0);
	// 三按钮中间项禁用时跨越到最后一个可用按钮
	const middleDisabled = (i: number) => i === 1;
	assert.equal(resolveDialogActionKey(key("ArrowRight"), 0, 3, middleDisabled), 2);
	assert.equal(resolveDialogActionKey(key("ArrowLeft"), 2, 3, middleDisabled), 0);
	// 全部禁用返回 null
	assert.equal(
		resolveDialogActionKey(key("ArrowRight"), 0, 2, () => true),
		null,
	);
	// IME / 修饰键 / 非方向键不拦截
	assert.equal(resolveDialogActionKey(key("ArrowRight", { isComposing: true }), 0, 2), null);
	assert.equal(resolveDialogActionKey(key("ArrowRight", { keyCode: 229 }), 0, 2), null);
	assert.equal(resolveDialogActionKey(key("ArrowRight", { ctrlKey: true }), 0, 2), null);
	assert.equal(resolveDialogActionKey(key("ArrowUp"), 0, 2), null);
});

test("列表与工具栏方向键纯函数支持夹紧、循环与禁用项跳过", () => {
	const key = (k: string) => ({
		key: k,
		defaultPrevented: false,
		isComposing: false,
		keyCode: 0,
		ctrlKey: false,
		metaKey: false,
		altKey: false,
		shiftKey: false,
	});
	assert.equal(resolveListNavKey(key("ArrowDown"), 0, 3), 1);
	assert.equal(resolveListNavKey(key("ArrowDown"), 2, 3), 2);
	assert.equal(resolveListNavKey(key("ArrowDown"), 2, 3, { wrap: true }), 0);
	assert.equal(resolveListNavKey(key("ArrowUp"), 0, 3, { wrap: true }), 2);
	assert.equal(resolveListNavKey(key("ArrowDown"), 0, 3, { isDisabled: (i) => i === 1 }), 2);
	assert.equal(resolveToolbarNavKey(key("ArrowLeft"), 0, 4, { wrap: true }), 3);
	assert.equal(resolveToolbarNavKey(key("ArrowRight"), 3, 4, { wrap: true }), 0);
});
