import assert from "node:assert/strict";
import { test } from "node:test";
import { groupSessions, orderedSessions, RECENT_SESSION_LIMIT } from "./session-list.ts";

test("会话按更新时间倒序，同时间保持顺序，不改动来源", () => {
	const rows = [
		{ id: "old", updatedAt: "2026-10-01T00:00:00Z" },
		{ id: "new", updatedAt: "2026-10-30T00:00:00Z" },
		{ id: "tie", updatedAt: "2026-10-30T00:00:00Z" },
		{ id: "missing" },
		{ id: "invalid", updatedAt: "invalid" },
	];
	assert.deepEqual(
		orderedSessions(rows).map((row) => row.id),
		["new", "tie", "old", "missing", "invalid"],
	);
	assert.equal(rows[0].id, "old");
	assert.deepEqual(orderedSessions([]), []);
});

test("最近二十项与完整列表使用相同次序，保留更早会话", () => {
	const rows = Array.from({ length: 25 }, (_, index) => ({
		id: String(index),
		updatedAt: new Date(2020, 0, index + 1).toISOString(),
	}));
	const ordered = orderedSessions(rows);
	assert.equal(RECENT_SESSION_LIMIT, 20);
	assert.equal(ordered.length, 25);
	assert.deepEqual(
		ordered.slice(0, RECENT_SESSION_LIMIT).map((row) => row.id),
		Array.from({ length: 20 }, (_, index) => String(24 - index)),
	);
});

test("全部会话按本地自然日分为互斥四组，保持倒序且每条只出现一次", () => {
	const now = new Date(2026, 10, 3, 12);
	const at = (offset: number, hour = 0) => new Date(2026, 10, 3 + offset, hour).toISOString();
	const rows = [
		{ id: "month-start", updatedAt: at(-29) },
		{ id: "today", updatedAt: at(0, 1) },
		{ id: "week-start", updatedAt: at(-6) },
		{ id: "yesterday", updatedAt: at(-1, 23) },
		{ id: "month", updatedAt: at(-7, 23) },
		{ id: "old", updatedAt: at(-30, 23) },
		{ id: "invalid", updatedAt: "invalid" },
		{ id: "missing" },
	];
	const groups = groupSessions(rows, now);
	assert.deepEqual(
		groups.map((group) => [group.label, group.sessions.map((row) => row.id)]),
		[
			["今天", ["today"]],
			["最近一周", ["yesterday", "week-start"]],
			["最近一个月", ["month", "month-start"]],
			["更早", ["old", "invalid", "missing"]],
		],
	);
	assert.equal(new Set(groups.flatMap((group) => group.sessions.map((row) => row.id))).size, rows.length);
	assert.equal(rows[0].id, "month-start", "不修改来源数组");
	assert.deepEqual(groupSessions([], now), [], "空列表不显示空分组");
	assert.equal(groupSessions([rows[1]], now).length, 1, "仅显示非空分组");
});
