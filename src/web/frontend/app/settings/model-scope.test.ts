import assert from "node:assert/strict";
import { test } from "node:test";
import { moveModel, normalizeScope, toggleModel } from "./model-scope.ts";

const all = ["a/one", "a/two", "b/three", "b/four"];
test("启用范围空值和全选沿用 Pi 默认，部分选择保留顺序并去重", () => {
	assert.equal(normalizeScope([], all), null);
	assert.equal(normalizeScope([...all].reverse(), all), null);
	assert.deepEqual(normalizeScope(["a/two", "a/two", "missing", "a/one"], all), ["a/two", "a/one"]);
	assert.equal(normalizeScope([], []), null);
});
test("默认范围取消勾选生成子集；最后一个取消或补齐所有项目恢复默认", () => {
	assert.deepEqual(toggleModel(null, all, "a/one"), all.slice(1));
	assert.equal(toggleModel(["a/two"], all, "a/two"), null);
	assert.equal(toggleModel(all.slice(1), all, "a/one"), null);
});
test("排序支持双向移动且不修改输入；非启用项目与默认范围保持不变", () => {
	const ids = all.slice(0, 3);
	assert.deepEqual(moveModel(ids, "a/one", "b/three"), ["a/two", "b/three", "a/one"]);
	assert.deepEqual(moveModel(ids, "b/three", "a/one"), ["b/three", "a/one", "a/two"]);
	assert.deepEqual(ids, all.slice(0, 3));
	assert.equal(moveModel(ids, "missing", "a/one"), ids);
	assert.equal(moveModel(ids, "a/one", "missing"), ids);
	assert.equal(moveModel(null, "a/one", "a/two"), null);
});
