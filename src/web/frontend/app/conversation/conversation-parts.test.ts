import assert from "node:assert/strict";
import { test } from "node:test";
import type { ContentPart, Message } from "../data/web-state.ts";
import {
	activeTurnIndex,
	formatDuration,
	groupTurns,
	isConversationMessage,
	orderedParts,
	splitTurn,
	type TurnEntry,
	visibleQueueItems,
} from "./conversation-parts.ts";

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
		["thinking", "text", "special", "text", "tools"],
	);
	assert.equal(parts[1].kind === "text" && parts[1].text, "下面联网搜索这两款的详细资料：");
	assert.equal(parts[4].kind === "tools" && parts[4].calls.length, 2);
});

test("助手附图保留顺序，并分隔相邻的工具分组", () => {
	const image: ContentPart = { type: "image", mimeType: "image/png", data: "fixture" };
	const parts = orderedParts([call("before"), image, call("after"), { type: "text", text: "最后正文" }]);
	assert.deepEqual(
		parts.map((part) => part.kind),
		["tools", "image", "tools", "text"],
	);
	assert.deepEqual(parts[1], { kind: "image", part: image });
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
		["thinking", "special", "text"],
	);
	assert.equal(finalText, "最后正文");
});

test("splitTurn keeps final prose visible when reasoning follows it in the provider message", () => {
	const { process, finalText } = splitTurn([
		entry(
			"assistant",
			1,
			[
				{ type: "text", text: "最后正文" },
				{ type: "thinking", thinking: "思考" },
			],
			"stop",
		),
	]);
	assert.equal(finalText, "最后正文");
	assert.deepEqual(
		process.map((item) => item.part.kind),
		["thinking"],
	);
	const unfinished = splitTurn([
		entry(
			"assistant",
			1,
			[{ type: "text", text: "检查前说明" }, { type: "thinking", thinking: "思考" }, call("a")],
			"toolUse",
		),
	]);
	assert.equal(unfinished.finalText, "");
	assert.deepEqual(
		unfinished.process.map((item) => item.part.kind),
		["text", "thinking", "tools"],
	);
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
	assert.equal(activeTurnIndex(steering, true), 1);
});

test("activeTurnIndex never hands the live turn to a truncated tail", () => {
	// /tree 回退到工具调用节点后分支以 toolUse 收尾，接着发新消息：运行中的是新轮次，
	// 旧轮次的计时从它自己的时间戳起算会横跨整段时间。
	const truncated = groupTurns([
		entry("user", 1_000, "先看一下磁盘"),
		entry("assistant", 1_100, [call("a", "disk")], "toolUse"),
		entry("user", 7_900_000, "继续"),
	]);
	assert.equal(activeTurnIndex(truncated, true), 1);
	assert.equal(activeTurnIndex(truncated, false), -1);
});

test("formatDuration rounds to seconds and switches to minutes past sixty", () => {
	assert.equal(formatDuration(300), "不足 1 秒");
	assert.equal(formatDuration(1200), "1 秒");
	assert.equal(formatDuration(59_000), "59 秒");
	assert.equal(formatDuration(65_000), "1 分 5 秒");
	assert.equal(formatDuration(120_000), "2 分钟");
});

test("isConversationMessage and groupTurns omit system metadata while preserving orphan assistant turns", () => {
	const rawSystemInit = {
		id: "sys-1",
		message: { role: "system", content: "", timestamp: 1 } as unknown as Message,
	};
	const rawSystemMid = {
		id: "sys-2",
		message: { role: "system", content: "", timestamp: 4 } as unknown as Message,
	};
	const rawToolResult = {
		id: "tr-1",
		message: { role: "toolResult" as const, toolCallId: "c1", content: "ok", timestamp: 3 },
	};

	assert.equal(isConversationMessage(rawSystemInit.message), false);
	assert.equal(isConversationMessage(rawToolResult.message), false);
	assert.equal(isConversationMessage(entry("user", 2, "问题").message), true);
	assert.equal(isConversationMessage(entry("assistant", 3, "回答").message), true);
	assert.equal(isConversationMessage(summary(5, "摘要").message), true);

	// Leading and mid-conversation system entries must never create a phantom leading turn or attach to assistants.
	const turnsWithSystem = groupTurns([
		rawSystemInit,
		entry("user", 2, "首轮提问"),
		entry("assistant", 3, "首轮回答", "stop"),
		rawSystemMid,
		entry("user", 5, "次轮提问"),
		entry("assistant", 6, "次轮回答", "stop"),
	]);
	assert.equal(turnsWithSystem.length, 2);
	assert.equal(turnsWithSystem[0].user?.message.timestamp, 2);
	assert.deepEqual(
		turnsWithSystem[0].assistants.map((item) => item.message.timestamp),
		[3],
	);
	assert.equal(turnsWithSystem[1].user?.message.timestamp, 5);
	assert.deepEqual(
		turnsWithSystem[1].assistants.map((item) => item.message.timestamp),
		[6],
	);

	// Legitimate leading orphan assistant messages (truncated history, tool-only, error, aborted) remain intact.
	const orphanTurns = groupTurns([
		rawSystemInit,
		entry("assistant", 10, [call("c-orphan")], "toolUse"),
		summary(11, "分支摘要"),
		entry("assistant", 12, "", "aborted"),
	]);
	assert.equal(orphanTurns.length, 3);
	assert.equal(orphanTurns[0].user, undefined);
	assert.deepEqual(
		orphanTurns[0].assistants.map((item) => item.message.timestamp),
		[10],
	);
	assert.equal(orphanTurns[1].summary?.message.summary, "分支摘要");
	assert.equal(orphanTurns[2].user, undefined);
	assert.deepEqual(
		orphanTurns[2].assistants.map((item) => item.message.timestamp),
		[12],
	);
});
