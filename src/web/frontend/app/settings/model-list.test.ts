import assert from "node:assert/strict";
import { test } from "node:test";
import { modelListRows } from "./model-list.ts";

const ids = Array.from({ length: 28 }, (_, i) => `model-${i}`);
test("模型列表默认显示六项，展开保留全部条目与顺序", () => {
	const collapsed = modelListRows(ids, ["missing-model"], false);
	assert.deepEqual(collapsed.ids, ids.slice(0, 6));
	assert.deepEqual(collapsed.unavailable, []);
	assert.equal(collapsed.total, 29);
	assert.equal(collapsed.hasMore, true);
	const expanded = modelListRows(ids, ["missing-model"], true);
	assert.deepEqual(expanded.ids, ids);
	assert.deepEqual(expanded.unavailable, ["missing-model"]);
	assert.deepEqual(modelListRows(ids, ["missing-model"], false), collapsed);
});
test("可用与不可用模型共用预览上限，短列表不提供多余折叠操作", () => {
	assert.deepEqual(modelListRows(ids.slice(0, 4), ["missing-a", "missing-b", "missing-c"], false), {
		ids: ids.slice(0, 4),
		unavailable: ["missing-a", "missing-b"],
		total: 7,
		hasMore: true,
	});
	assert.equal(modelListRows(ids.slice(0, 3), ["missing-model"], false).hasMore, false);
	assert.deepEqual(modelListRows([], [], false), { ids: [], unavailable: [], total: 0, hasMore: false });
});
