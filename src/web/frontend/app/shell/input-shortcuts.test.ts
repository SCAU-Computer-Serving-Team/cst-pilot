import assert from "node:assert/strict";
import test from "node:test";
import { isFocusInputShortcut, isModelPickerShortcut, isPlainVerticalArrow } from "./input-shortcuts.ts";

test("Ctrl+J 快捷键匹配逻辑排除多余修饰键与输入法组合态", () => {
	assert.equal(
		isFocusInputShortcut({ ctrlKey: true, altKey: false, metaKey: false, shiftKey: false, key: "j" }),
		true,
	);
	assert.equal(
		isFocusInputShortcut({ ctrlKey: true, altKey: false, metaKey: false, shiftKey: false, key: "J", code: "KeyJ" }),
		true,
	);
	assert.equal(
		isFocusInputShortcut({ ctrlKey: true, altKey: false, metaKey: false, shiftKey: true, key: "J" }),
		false,
	);
	assert.equal(
		isFocusInputShortcut({
			ctrlKey: true,
			altKey: false,
			metaKey: false,
			shiftKey: false,
			key: "j",
			isComposing: true,
		}),
		false,
	);
	assert.equal(
		isFocusInputShortcut({ ctrlKey: false, altKey: false, metaKey: true, shiftKey: false, key: "j" }),
		false,
	);
});

test("纯上下方向键判定排除 Shift 选区与其他修饰键", () => {
	assert.equal(
		isPlainVerticalArrow({ key: "ArrowUp", shiftKey: false, ctrlKey: false, altKey: false, metaKey: false }),
		"up",
	);
	assert.equal(
		isPlainVerticalArrow({ key: "ArrowDown", shiftKey: false, ctrlKey: false, altKey: false, metaKey: false }),
		"down",
	);
	assert.equal(
		isPlainVerticalArrow({ key: "ArrowUp", shiftKey: true, ctrlKey: false, altKey: false, metaKey: false }),
		null,
	);
	assert.equal(
		isPlainVerticalArrow({
			key: "ArrowDown",
			shiftKey: false,
			ctrlKey: false,
			altKey: false,
			metaKey: false,
			isComposing: true,
		}),
		null,
	);
});

test("Alt+M 快捷键匹配逻辑排除多余修饰键与输入法组合态", () => {
	assert.equal(
		isModelPickerShortcut({ ctrlKey: false, altKey: true, metaKey: false, shiftKey: false, key: "m" }),
		true,
	);
	assert.equal(
		isModelPickerShortcut({ ctrlKey: false, altKey: true, metaKey: false, shiftKey: false, key: "M", code: "KeyM" }),
		true,
	);
	assert.equal(
		isModelPickerShortcut({ ctrlKey: true, altKey: true, metaKey: false, shiftKey: false, key: "m" }),
		false,
	);
	assert.equal(
		isModelPickerShortcut({
			ctrlKey: false,
			altKey: true,
			metaKey: false,
			shiftKey: false,
			key: "m",
			isComposing: true,
		}),
		false,
	);
});
