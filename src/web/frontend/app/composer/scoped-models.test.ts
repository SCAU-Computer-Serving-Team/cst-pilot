import assert from "node:assert/strict";
import { test } from "node:test";
import { scopedModels } from "./scoped-models.ts";

const models = ["one", "two", "three"].map((id) => ({ id, provider: "probe", name: id, reasoning: false }));
test("模型选择只列 scoped 范围，使用 TUI 保存的顺序", () => {
	assert.deepEqual(
		scopedModels(models, ["probe/three", "probe/one"]).map((model) => model.id),
		["three", "one"],
	);
	assert.deepEqual(scopedModels(models, []), []);
	assert.deepEqual(scopedModels(models, ["probe/missing"]), []);
	assert.deepEqual(scopedModels(models, null), models);
});
