import assert from "node:assert/strict";
import { test } from "node:test";
import type { ContentPart, Message } from "../data/web-state.ts";
import { isStandaloneTool, splitToolCalls, toolState } from "./tool-state.ts";

const call = (name: string, scope?: string): Extract<ContentPart, { type: "toolCall" }> => ({
	type: "toolCall",
	id: "call",
	name,
	arguments: scope ? { scope } : {},
});
const result: Message = { role: "toolResult", timestamp: 1, content: [{ type: "text", text: "完成" }] };

test("only a live call with no result spins", () => {
	assert.equal(toolState(undefined, undefined, true), "running");
	assert.equal(toolState(undefined, undefined, true, false), "pending");
	assert.equal(toolState(result, "success", true, false), "success");
	assert.equal(toolState(undefined, undefined, false), "interrupted");
	assert.equal(toolState(undefined, undefined), "interrupted");
	assert.equal(toolState(result, "success", true), "success");
});

test("failure and partial results keep separate visual states", () => {
	assert.equal(toolState({ ...result, isError: true }, "success"), "error");
	assert.equal(toolState(result, "error"), "error");
	assert.equal(toolState(result, "degraded"), "degraded");
});

test("canvas exceptions render outside the collapsed tool group", () => {
	for (const value of [call("web_search"), call("runbook"), call("sys"), call("sys", "overview")]) {
		assert.equal(isStandaloneTool(value, result), true);
	}
	for (const value of [call("disk"), call("sys", "proc"), call("read"), call("fffind")]) {
		assert.equal(isStandaloneTool(value, result), false);
	}
});

test("standalone tools keep their position among grouped calls", () => {
	const groups = splitToolCalls([call("disk"), call("web_search"), call("sys", "proc"), call("disk")], new Map());
	assert.deepEqual(
		groups.map((group) => [group.kind, group.calls.length]),
		[
			["group", 1],
			["standalone", 1],
			["group", 2],
		],
	);
});

test("read images stay visible while text reads remain collapsible", () => {
	const image: Message = { ...result, content: [{ type: "image", mimeType: "image/png", data: "AA==" }] };
	assert.equal(isStandaloneTool(call("read"), image), true);
	assert.equal(isStandaloneTool(call("read"), result), false);
	assert.equal(isStandaloneTool(call("read")), false);
	assert.equal(isStandaloneTool(call("disk"), image), false);
});
