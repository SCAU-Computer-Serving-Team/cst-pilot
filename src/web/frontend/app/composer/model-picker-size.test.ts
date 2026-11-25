import assert from "node:assert/strict";
import { test } from "node:test";
import {
	computeModelPickerHeight,
	computeModelPickerWidth,
	modelDisplayName,
	PICKER_MAX_WIDTH,
	PICKER_MIN_WIDTH,
} from "./model-picker-size.ts";

test("模型选择器主面板与思考强度子页至少为 224px，长名称可自适应扩展至 320px", () => {
	const shortMain = computeModelPickerWidth({
		page: "main",
		selectedLabel: "Mini",
		thinkingLabel: "High",
		models: [],
		thinkingLabels: ["Low", "Medium", "High", "Max"],
	});
	assert.equal(shortMain, PICKER_MIN_WIDTH);

	const defaultMain = computeModelPickerWidth({
		page: "main",
		selectedLabel: "DeepSeek V4.1 Flash",
		thinkingLabel: "Max",
		models: [],
		thinkingLabels: ["Low", "Medium", "High", "Max"],
	});
	assert.ok(defaultMain >= PICKER_MIN_WIDTH && defaultMain <= PICKER_MAX_WIDTH);

	const longMain = computeModelPickerWidth({
		page: "main",
		selectedLabel: "DeepSeek V4.1 Long Reasoning Display Name",
		thinkingLabel: "High",
		models: [],
		thinkingLabels: ["Low", "Medium", "High", "Max"],
	});
	assert.ok(longMain > defaultMain && longMain <= PICKER_MAX_WIDTH);

	const thinkingWidth = computeModelPickerWidth({
		page: "thinking",
		selectedLabel: "DeepSeek V4.1 Flash",
		thinkingLabel: "High",
		models: [],
		thinkingLabels: ["Off", "Minimal", "Low", "Medium", "High", "Max"],
	});
	assert.equal(thinkingWidth, PICKER_MIN_WIDTH);
});

test("模型列表按模型名与服务提供商宽度自适应，并受 224–320px 上下限约束", () => {
	const compactWidth = computeModelPickerWidth({
		page: "models",
		selectedLabel: "Mini",
		thinkingLabel: "High",
		models: [{ id: "mini", name: "Mini", provider: "cstoa" }],
		thinkingLabels: ["High"],
	});
	assert.equal(compactWidth, PICKER_MIN_WIDTH);

	const providerWidth = computeModelPickerWidth({
		page: "models",
		selectedLabel: "DeepSeek V4.1 Flash",
		thinkingLabel: "High",
		models: [
			{ id: "deepseek-flash", name: "DeepSeek V4.1 Flash", provider: "cstoa" },
			{ id: "qwen3-coder-plus", name: "Qwen3 Coder Plus", provider: "qwen-token-plan-cn" },
		],
		thinkingLabels: ["High"],
	});
	assert.ok(providerWidth > PICKER_MIN_WIDTH && providerWidth <= PICKER_MAX_WIDTH);

	const extremeWidth = computeModelPickerWidth({
		page: "models",
		selectedLabel: "Long",
		thinkingLabel: "High",
		models: [
			{
				id: "long",
				name: "用于验证超长名称截断的模型名称 Very Long Model Display Name",
				provider: "custom-long-provider-gateway",
			},
		],
		thinkingLabels: ["High"],
	});
	assert.equal(extremeWidth, PICKER_MAX_WIDTH);
});

test("空白模型显示名回退到模型 ID，且高度按行数计算", () => {
	assert.equal(modelDisplayName({ id: "deepseek-flash", name: "   " }), "deepseek-flash");
	assert.equal(modelDisplayName({ id: "deepseek-flash", name: "DeepSeek V4.1 Flash" }), "DeepSeek V4.1 Flash");
	assert.equal(computeModelPickerHeight("main", 3), 74);
	assert.equal(computeModelPickerHeight("models", 3), 142);
});
