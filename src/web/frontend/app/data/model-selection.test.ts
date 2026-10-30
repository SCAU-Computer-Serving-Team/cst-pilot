import assert from "node:assert/strict";
import { test } from "node:test";
import { type ModelChoice, type ModelSelectionPayload, refreshManualSelection } from "./model-selection.ts";

const defaultModel: ModelChoice = {
	provider: "deepseek",
	id: "deepseek-flash",
	name: "DeepSeek V4.1 Flash",
	reasoning: true,
	credentialReady: false,
	catalogPresent: true,
};
const other: ModelChoice = { ...defaultModel, provider: "probe", id: "one", name: "Other", credentialReady: true };
const data: ModelSelectionPayload = {
	models: [other],
	catalog: [defaultModel, other],
	defaultModel,
	selected: defaultModel,
	enabled: null,
	unavailableEnabled: [],
};

test("无密钥默认模型保持身份，不采用可用清单第一项", () => {
	assert.equal(refreshManualSelection(null, data, false), defaultModel);
	assert.equal(refreshManualSelection(other, data, false), defaultModel);
});

test("凭据与目录更新只刷新用户手动选择的模型", () => {
	assert.equal(refreshManualSelection(other, data, true), other);
	const changed = { ...other, credentialReady: false };
	assert.equal(refreshManualSelection(other, { ...data, catalog: [defaultModel, changed] }, true), changed);
	assert.deepEqual(refreshManualSelection(other, { ...data, catalog: [defaultModel] }, true), {
		...other,
		credentialReady: false,
		catalogPresent: false,
	});
});
