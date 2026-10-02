import assert from "node:assert/strict";
import { test } from "node:test";
import {
	activeTurnIndex,
	formatDuration,
	groupTurns,
	orderedParts,
	splitTurn,
	type TurnEntry,
	visibleQueueItems,
} from "./conversation-parts.ts";
import type { ContentPart, Message } from "./web-state.ts";

const call = (id: string, name = "disk"): ContentPart => ({ type: "toolCall", id, name, arguments: {} });
test("assistant prose stays before and after the corresponding tool calls", () => {
	const parts = orderedParts([
		{ type: "thinking", thinking: "why" },
		{ type: "text", text: "下面联网搜索这两款的详细资料：" },
		call("a", "web_search"),
		{ type: "text", text: "查询后的结论" },
		call("b"),
		call("c"),
	]);
	assert.deepEqual(
		parts.map((part) => part.kind),
		["thinking", "text", "tools", "text", "tools"],
	);
	assert.equal(parts[1].kind === "text" && parts[1].text, "下面联网搜索这两款的详细资料：");
	assert.equal(parts[4].kind === "tools" && parts[4].calls.length, 2);
});

test("a direct submission is never rendered as a queued follow-up", () => {
	const make = (status: "pending" | "failed", id: string, delivery: "direct" | "queue") => ({
		id,
		status,
		delivery,
		text: id,
		sequence: 1,
		images: [],
	});
	assert.deepEqual(visibleQueueItems([make("pending", "first", "direct")]), []);
	assert.deepEqual(
		visibleQueueItems([make("pending", "later", "queue")]).map((item) => item.id),
		["later"],
	);
	assert.deepEqual(
		visibleQueueItems([make("pending", "first", "direct"), make("pending", "later", "queue")]).map((item) => item.id),
		["later"],
	);
	assert.deepEqual(
		visibleQueueItems([make("failed", "no-message", "direct")]).map((item) => item.id),
		["no-message"],
	);
});

const entry = (
	role: "user" | "assistant",
	timestamp: number,
	content: Message["content"],
	stopReason?: string,
): TurnEntry => ({ message: { role, content, timestamp, ...(stopReason ? { stopReason } : {}) } });

test("groupTurns groups assistant messages under the preceding user message", () => {
	const turns = groupTurns([
		entry("user", 1, "你好"),
		entry("assistant", 2, "收到"),
		entry("assistant", 3, "继续"),
		entry("user", 4, "再来"),
	]);
	assert.equal(turns.length, 2);
	assert.deepEqual(
		turns[0].assistants.map((item) => item.message.timestamp),
		[2, 3],
	);
	assert.equal(turns[1].assistants.length, 0);
});

const summary = (timestamp: number, text: string): TurnEntry => ({
	message: { role: "branchSummary", content: "", summary: text, timestamp },
});

test("a branch summary is its own block and never swallows the message after it", () => {
	const turns = groupTurns([entry("user", 1, "旧分支"), summary(2, "小结"), entry("assistant", 3, "新分支的回答")]);
	assert.equal(turns.length, 3);
	assert.deepEqual(turns[1].assistants, []);
	assert.equal(turns[1].summary?.message.summary, "小结");
	assert.equal(turns[2].summary, undefined);
	assert.equal(turns[2].assistants.length, 1);
});

test("branch summary blocks do not shift the live turn", () => {
	assert.equal(activeTurnIndex(groupTurns([entry("user", 1, "问题"), summary(2, "小结")]), true), 0);
	const turns = groupTurns([
		entry("user", 1, "问题"),
		entry("assistant", 2, "答", "endTurn"),
		summary(3, "小结"),
		entry("user", 4, "追问"),
	]);
	assert.equal(turns.length, 3);
	assert.equal(activeTurnIndex(turns, true), 2);
});

test("splitTurn keeps only the trailing text of the last assistant message outside the process", () => {
	const { process, finalText } = splitTurn([
		entry(
			"assistant",
			1,
			[{ type: "thinking", thinking: "t" }, call("a", "web_search"), { type: "text", text: "中间正文" }],
			"toolUse",
		),
		entry("assistant", 2, [{ type: "text", text: "最后正文" }], "stop"),
	]);
	assert.deepEqual(
		process.map((item) => item.part.kind),
		["thinking", "tools", "text"],
	);
	assert.equal(finalText, "最后正文");
});

test("splitTurn leaves everything in the process when the turn has no final text", () => {
	const { process, finalText } = splitTurn([entry("assistant", 1, [call("a")], "toolUse")]);
	assert.equal(process.length, 1);
	assert.equal(finalText, "");
});

test("activeTurnIndex follows the running turn", () => {
	const waiting = groupTurns([entry("user", 1, "一"), entry("assistant", 2, "答", "stop"), entry("user", 3, "二")]);
	assert.equal(activeTurnIndex(waiting, true), 1);
	assert.equal(activeTurnIndex(waiting, false), -1);
	const steering = groupTurns([
		entry("user", 1, "一"),
		entry("assistant", 2, [call("a")], "toolUse"),
		entry("user", 3, "插话"),
	]);
	assert.equal(activeTurnIndex(steering, true), 0);
});

test("formatDuration rounds to seconds and switches to minutes past sixty", () => {
	assert.equal(formatDuration(300), "不足 1 秒");
	assert.equal(formatDuration(1200), "1 秒");
	assert.equal(formatDuration(59_000), "59 秒");
	assert.equal(formatDuration(65_000), "1 分 5 秒");
	assert.equal(formatDuration(120_000), "2 分钟");
});
