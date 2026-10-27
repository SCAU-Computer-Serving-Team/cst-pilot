import assert from "node:assert/strict";
import { test } from "node:test";
import { commitMessage, containsAssistant } from "./session-stream.ts";
import type { Message, SessionDetail } from "./web-state.ts";

const state = (): SessionDetail => ({
	id: "a",
	messages: [{ role: "user", content: "问题", timestamp: 1 }],
	entries: [{ id: "u", message: { role: "user", content: "问题", timestamp: 1 } }],
	running: true,
	queue: { version: 0, items: [], paused: false },
	ui: [],
});
test("结束事件直接承接完整正文，重放不重复，并保留服务端条目 ID", () => {
	const end: Message = { role: "assistant", content: "正文完整结论", timestamp: 2, stopReason: "stop" };
	const first = commitMessage(state(), end);
	assert.equal(first.entries.at(-1)?.message.content, end.content);
	assert.equal(containsAssistant(first, end), true);
	const repeated = commitMessage(first, end);
	assert.equal(repeated.messages.length, 2);
	assert.equal(repeated.entries.length, 2);
	first.entries[1].id = "persisted";
	assert.equal(commitMessage(first, end).entries[1].id, "persisted");
	const user: Message = { role: "user", content: "下一轮", timestamp: 3 };
	const accepted = commitMessage(first, user);
	assert.equal(accepted.entries.at(-1)?.message, user, "新一轮用户消息先进入展示，再承接快速回复");
	assert.equal(commitMessage(accepted, user).messages.length, 3);
});
