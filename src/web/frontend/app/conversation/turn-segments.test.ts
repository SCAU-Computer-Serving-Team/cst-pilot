import assert from "node:assert/strict";
import { test } from "node:test";
import type { Message } from "../data/web-state.ts";
import { splitTurn, type TurnEntry } from "./conversation-parts.ts";
import { completedToolCalls, turnSegments } from "./turn-segments.ts";

const entry: TurnEntry = {
	id: "entry",
	message: {
		role: "assistant",
		timestamp: 1,
		content: [
			{ type: "toolCall", name: "read", id: "read-1", arguments: {} },
			{ type: "toolCall", name: "sys", id: "sys", arguments: {} },
			{ type: "text", text: "中间正文" },
			{ type: "toolCall", name: "read", id: "read-2", arguments: {} },
			{ type: "text", text: "最终正文" },
		],
	},
};
test("特殊工具两侧过程分开，正文归属保持规格", () => {
	const split = splitTurn([entry]);
	assert.equal(split.finalText, "最终正文");
	const segments = turnSegments(split.process);
	assert.deepEqual(
		segments.map((x) => x.kind),
		["process", "visible", "process"],
	);
	assert.deepEqual(
		segments[2]?.parts.map((x) => x.part.kind),
		["text", "tools"],
	);
});
test("过程计数只统计本段已完成的普通工具调用", () => {
	const segments = turnSegments(splitTurn([entry]).process);
	const done: Message = { role: "toolResult", timestamp: 2, content: "done" };
	const results = new Map<string, Map<string, Message>>([
		[
			"entry",
			new Map([
				["read-1", done],
				["sys", done],
			]),
		],
	]);
	assert.equal(completedToolCalls(segments[0]!.parts, results), 1);
	assert.equal(completedToolCalls(segments[1]!.parts, results), 0);
	assert.equal(completedToolCalls(segments[2]!.parts, results), 0);
	results.get("entry")!.set("read-2", { ...done, isError: true });
	assert.equal(completedToolCalls(segments[2]!.parts, results), 1);
	assert.equal(completedToolCalls(segments[2]!.parts, new Map([["other-entry", results.get("entry")!]])), 0);
	results
		.get("entry")!
		.set("read-2", { ...done, content: [{ type: "image", data: "fixture", mimeType: "image/png" }] });
	assert.equal(completedToolCalls(segments[2]!.parts, results), 0);
});
test("纯正文与仅特殊工具轮次保留工作时长入口", () => {
	assert.equal(turnSegments([])[0]?.key, "process-start");
	const segments = turnSegments(
		splitTurn([
			{
				...entry,
				message: { ...entry.message, content: [{ type: "toolCall", name: "sys", id: "s", arguments: {} }] },
			},
		]).process,
	);
	assert.deepEqual(
		segments.map((x) => x.kind),
		["process", "visible"],
	);
});
