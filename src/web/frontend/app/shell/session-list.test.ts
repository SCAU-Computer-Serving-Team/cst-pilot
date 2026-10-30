import assert from "node:assert/strict";
import { test } from "node:test";
import { orderedSessions, RECENT_SESSION_LIMIT } from "./session-list.ts";

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

test("最近十项与完整列表使用相同次序，保留更早会话", () => {
	const rows = Array.from({ length: 15 }, (_, index) => ({
		id: String(index),
		updatedAt: new Date(2020, 0, index + 1).toISOString(),
	}));
	const ordered = orderedSessions(rows);
	assert.equal(RECENT_SESSION_LIMIT, 10);
	assert.equal(ordered.length, 15);
	assert.deepEqual(
		ordered.slice(0, RECENT_SESSION_LIMIT).map((row) => row.id),
		Array.from({ length: 10 }, (_, index) => String(14 - index)),
	);
});
