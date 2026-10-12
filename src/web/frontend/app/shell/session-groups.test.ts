import assert from "node:assert/strict";
import { test } from "node:test";
import { dayGroup, groupSelection, sessionGroups, toggleSessionGroup } from "./session-groups.ts";

const now = new Date(2026, 9, 11, 12);
const ago = (days: number) => new Date(2026, 9, 11 - days, 23, 59).toISOString();

test("日期分组互斥，覆盖一个月边界", () => {
	assert.deepEqual(sessionGroups, ["今天", "昨天", "一周内", "一个月内", "更早"]);
	for (const [days, expected] of [
		[0, "今天"],
		[1, "昨天"],
		[2, "一周内"],
		[7, "一周内"],
		[8, "一个月内"],
		[30, "一个月内"],
		[31, "更早"],
	] as const) {
		assert.equal(dayGroup(ago(days), now), expected);
	}
	assert.equal(dayGroup(undefined, now), "更早");
	assert.equal(dayGroup("invalid", now), "更早");
	assert.equal(dayGroup(ago(-1), now), "今天");
});

test("整组选择、半选补齐和清空不影响其他组", () => {
	const ids = ["a", "b"];
	const original = new Set(["a", "other"]);
	assert.deepEqual(groupSelection(ids, original), { checked: false, mixed: true });
	const all = toggleSessionGroup(ids, original);
	assert.deepEqual(all, new Set(["a", "other", "b"]));
	assert.deepEqual(original, new Set(["a", "other"]));
	assert.deepEqual(groupSelection(ids, all), { checked: true, mixed: false });
	assert.deepEqual(toggleSessionGroup(ids, all), new Set(["other"]));
	assert.deepEqual(groupSelection(ids, null), { checked: false, mixed: false });
	assert.deepEqual(groupSelection([], all), { checked: false, mixed: false });
});
