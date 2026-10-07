import assert from "node:assert/strict";
import { test } from "node:test";
import type { Message } from "../data/web-state.ts";
import { matchToolResults } from "./match-results.ts";

const call = (timestamp: number): Message => ({
	role: "assistant",
	timestamp,
	content: [{ type: "toolCall", id: "reused", name: "disk", arguments: {} }],
});
const result = (timestamp: number, content: string): Message => ({
	role: "toolResult",
	timestamp,
	toolCallId: "reused",
	content,
});

test("reused provider call IDs map to their own assistant turns", () => {
	const first = result(2, "first result");
	const second = result(4, "second result");
	const matches = matchToolResults([
		{ id: "turn-1", message: call(1) },
		{ message: first },
		{ id: "turn-2", message: call(3) },
		{ message: second },
	]);
	assert.equal(matches.get("turn-1")?.get("reused"), first);
	assert.equal(matches.get("turn-2")?.get("reused"), second);
});

test("独立工具结束事件结果立即对应当前调用，后续落盘结果替换它", () => {
	const fast = result(2, "fast");
	const message = call(1);
	if (Array.isArray(message.content) && message.content[0]?.type === "toolCall")
		message.content[0].execution = { startedAt: 1, endedAt: 2, result: fast };
	const live = matchToolResults([{ id: "turn", message }]);
	assert.equal(live.get("turn")?.get("reused"), fast);
	const persisted = result(5, "fast");
	const done = matchToolResults([{ id: "turn", message }, { message: persisted }]);
	assert.equal(done.get("turn")?.get("reused"), persisted);
});

test("unmatched results do not attach to an unrelated later turn", () => {
	const matches = matchToolResults([{ message: result(1, "orphan") }, { id: "turn", message: call(2) }]);
	assert.equal(matches.get("turn")?.size, 0);
});
