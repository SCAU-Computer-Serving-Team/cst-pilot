import assert from "node:assert/strict";
import { test } from "node:test";
import { OptimisticSessionDeletion } from "./optimistic-session-deletion.ts";

const rows = [
	{ id: "a", title: "今天", updatedAt: "2026-10-12T12:00:00Z", running: false },
	{ id: "b", title: "昨天", updatedAt: "2026-10-11T12:00:00Z", running: true },
	{ id: "c", title: "较早", updatedAt: "2026-10-01T12:00:00Z", running: false },
];

test("确认后立即隐藏；期间任何服务端列表都不能恢复待删除项", () => {
	const transaction = new OptimisticSessionDeletion();
	assert.deepEqual(transaction.begin(rows, ["a", "b"]), [rows[2]]);
	assert.deepEqual(transaction.reconcile(rows), [rows[2]]);
});

test("部分失败只恢复失败项和原排序，保留最新名称与运行状态", () => {
	const transaction = new OptimisticSessionDeletion();
	const visible = transaction.begin(rows, ["a", "b"]);
	transaction.reconcile([rows[0], { ...rows[1], title: "已改名", running: false }, rows[2]]);
	assert.deepEqual(transaction.settle(visible, ["a"], ["b"]), [
		{ ...rows[1], title: "已改名", running: false },
		rows[2],
	]);
	assert.deepEqual(
		transaction.reconcile(rows).map((row) => row.id),
		["b", "c"],
		"成功项不被旧结果复活",
	);
});

test("全部失败恢复完整快照，随后真实刷新接管列表", () => {
	const transaction = new OptimisticSessionDeletion();
	assert.deepEqual(transaction.settle(transaction.begin(rows, ["a", "b", "c"]), [], ["a", "b", "c"]), rows);
	assert.deepEqual(transaction.reconcile([rows[1]]), [rows[1]]);
});

test("其他会话新增不会被回滚覆盖，成功删除仅清理对应会话", () => {
	const transaction = new OptimisticSessionDeletion();
	transaction.begin(rows, ["a", "b"]);
	const newest = { id: "new", title: "新会话", updatedAt: "2026-10-12T13:00:00Z", running: false };
	const visible = transaction.reconcile([newest, ...rows]);
	assert.deepEqual(transaction.settle(visible, ["a"], ["b"]), [newest, rows[1], rows[2]]);
});
