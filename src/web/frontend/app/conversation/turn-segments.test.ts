import assert from "node:assert/strict";
import { test } from "node:test";
import type { ContentPart, Message } from "../data/web-state.ts";
import { splitTurn, type TurnEntry, type TurnPart } from "./conversation-parts.ts";
import { processSegmentLabel, turnSegments } from "./turn-segments.ts";

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
const done: Message = { role: "toolResult", timestamp: 2, content: "done" };
const empty = new Map<string, Map<string, Message>>();
const partsOf = (content: ContentPart[]): TurnPart[] =>
	splitTurn([{ ...entry, message: { ...entry.message, content } }]).process;

test("特殊工具两侧过程分开，正文归属保持规格", () => {
	const split = splitTurn([entry]);
	assert.equal(split.finalText, "最终正文");
	const segments = turnSegments(split.process);
	assert.deepEqual(
		segments.map((segment) => segment.kind),
		["process", "visible", "process"],
	);
	assert.deepEqual(
		segments[2]?.parts.map((part) => part.part.kind),
		["text", "tools"],
	);
});

test("过程计数只统计本段普通工具，失败调用计入已结束次数，图片不计入", () => {
	const segments = turnSegments(splitTurn([entry]).process);
	const results = new Map([
		[
			"entry",
			new Map([
				["read-1", done],
				["sys", done],
			]),
		],
	]);
	assert.equal(processSegmentLabel(segments[0]!.parts, results, false), "已完成 1 次工具调用");
	assert.equal(processSegmentLabel(segments[1]!.parts, results, false), undefined);
	assert.equal(processSegmentLabel(segments[2]!.parts, results, false), "检查未完成");
	results.get("entry")!.set("read-2", { ...done, isError: true });
	assert.equal(processSegmentLabel(segments[2]!.parts, results, false), "已完成 1 次工具调用");
	assert.equal(
		processSegmentLabel(segments[2]!.parts, new Map([["other-entry", results.get("entry")!]]), false),
		"检查未完成",
	);
	results
		.get("entry")!
		.set("read-2", { ...done, content: [{ type: "image", data: "fixture", mimeType: "image/png" }] });
	assert.equal(processSegmentLabel(segments[2]!.parts, results, false), "处理过程");
});

test("未返回的普通工具显示进行中或中断，部分完成保留实际进度", () => {
	const parts = partsOf([
		{ type: "toolCall", name: "read", id: "read-1", arguments: {} },
		{ type: "toolCall", name: "disk", id: "disk", arguments: {} },
	]);
	assert.equal(processSegmentLabel(parts, empty, true), "正在调用 2 个工具");
	assert.equal(processSegmentLabel(parts, empty, false), "检查未完成");
	const results = new Map([["entry", new Map([["read-1", done]])]]);
	assert.equal(processSegmentLabel(parts, results, true), "已完成 1 / 2 次工具调用");
	assert.equal(processSegmentLabel(parts, results, false), "已完成 1 / 2 次工具调用");
	results.get("entry")!.set("disk", { ...done, isError: true });
	assert.equal(processSegmentLabel(parts, results, false), "已完成 2 次工具调用");
});

for (const [name, args] of [
	["web_search", {}],
	["sys", {}],
	["sys", { scope: "overview" }],
	["runbook", {}],
	["command_plan", {}],
	["runbook_deliver", {}],
	["bitlocker", { scope: "unlock" }],
	["bitlocker", { scope: "decrypt" }],
] as const) {
	test(`${name} ${"scope" in args ? args.scope : ""}：独立呈现，后续思考与正文不显示零次工具调用`, () => {
		const content: ContentPart[] = [
			{ type: "toolCall", name, id: "special-1", arguments: args },
			{ type: "thinking", thinking: "保留工具之后的思考" },
			{ type: "toolCall", name, id: "special-2", arguments: args },
			{ type: "text", text: "保留中间正文" },
			{ type: "thinking", thinking: "" },
			{ type: "text", text: "最终回答" },
		];
		const split = splitTurn([{ ...entry, message: { ...entry.message, content } }]);
		assert.equal(split.finalText, "最终回答");
		const segments = turnSegments(split.process);
		assert.deepEqual(
			segments.map((segment) => segment.kind),
			["process", "visible", "process", "visible", "process"],
		);
		assert.equal(segments[0]?.key, "process-start");
		for (const live of [true, false]) {
			assert.equal(processSegmentLabel(segments[2]!.parts, empty, live), "思考过程");
			assert.equal(processSegmentLabel(segments[4]!.parts, empty, live), "处理过程");
		}
	});
}

test("空的后续过程不创建入口；已脱敏的思考保留不可用提示", () => {
	const parts = partsOf([{ type: "thinking", thinking: "\n " }]);
	assert.equal(processSegmentLabel(parts, empty, true), undefined);
	assert.equal(processSegmentLabel(parts, empty, false), undefined);
	const redacted = partsOf([{ type: "thinking", thinking: "", redacted: true }]);
	assert.equal(processSegmentLabel(redacted, empty, false), "思考过程");
});

test("纯正文与仅特殊工具轮次保留工作时长入口", () => {
	assert.equal(turnSegments([])[0]?.key, "process-start");
	const segments = turnSegments(partsOf([{ type: "toolCall", name: "sys", id: "s", arguments: {} }]));
	assert.deepEqual(
		segments.map((segment) => segment.kind),
		["process", "visible"],
	);
});
