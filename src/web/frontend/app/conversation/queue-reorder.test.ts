import assert from "node:assert/strict";
import { test } from "node:test";
import type { InboxSnapshot } from "../data/web-state.ts";
import { acceptNewerQueue, queuedIdsOf, reconcilePendingOrder } from "./queue-reorder.ts";

function makeQueue(version: number, ids: string[]): InboxSnapshot {
	return {
		version,
		paused: false,
		items: ids.map((id, index) => ({
			id,
			sequence: index + 1,
			delivery: "queue",
			status: "pending",
			text: `msg-${id}`,
			images: [],
			createdAt: 0,
		})),
	};
}

test("HTTP 响应先于 SSE 到达时接纳新版本队列并忽略旧版本推送", () => {
	const initial = makeQueue(7, ["q1", "q2", "q3"]);
	const fromHttp = makeQueue(8, ["q1", "q3", "q2"]);
	const confirmed = acceptNewerQueue(initial, fromHttp);
	assert.equal(confirmed.version, 8);
	assert.deepEqual(queuedIdsOf(confirmed), ["q1", "q3", "q2"]);
	assert.equal(acceptNewerQueue(confirmed, initial).version, 8);
});

test("连按排序意图基于最新已确认快照对账并跳过冗余写入", () => {
	const afterFirst = makeQueue(8, ["q1", "q3", "q2"]);
	assert.deepEqual(reconcilePendingOrder(afterFirst, ["q3", "q1", "q2"]), ["q3", "q1", "q2"]);
	assert.equal(reconcilePendingOrder(afterFirst, ["q1", "q3", "q2"]), null);
});

test("队列集合并发变化时剔除消失条目并保留新增条目", () => {
	const concurrent = makeQueue(9, ["q2", "q3", "q4"]);
	assert.deepEqual(reconcilePendingOrder(concurrent, ["q3", "q1", "q2"]), ["q3", "q2", "q4"]);
});
