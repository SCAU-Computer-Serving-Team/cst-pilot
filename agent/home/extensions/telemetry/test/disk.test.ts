import assert from "node:assert/strict";
import { test } from "node:test";
import { newState, onToolCall, onToolExecutionEnd, onToolExecutionStart } from "../collect.ts";

test("存储与检索工具仅在通道回退时计 degraded，在顶层裁剪时计 truncated", () => {
	const state = newState("fixture", "new", "web");
	for (const [id, usage] of Object.entries({
		complete: { method: "native-walk", truncated: false },
		partial: { method: "native-walk", truncated: true, denied: ["fixture"] },
		fallback: { method: "native-walk", degradedFrom: "native-mft: 权限不足", truncated: true },
	})) {
		onToolCall(state, id, "disk", { scope: "usage" });
		onToolExecutionStart(state, id, "disk");
		onToolExecutionEnd(state, id, { details: { usage } }, false);
	}
	assert.equal(state.tools.get("disk(usage)")?.calls, 3);
	assert.equal(state.tools.get("disk(usage)")?.degraded, 1);
	assert.equal(state.tools.get("disk(usage)")?.truncated, 0);

	onToolExecutionStart(state, "ls-ok", "ls");
	onToolExecutionEnd(state, "ls-ok", { details: { method: "walk", entries: [] } }, false);
	onToolExecutionStart(state, "ls-fb", "ls");
	onToolExecutionEnd(state, "ls-fb", { details: { method: "walk", degradedFrom: "mft", entries: [] } }, false);
	assert.equal(state.tools.get("ls")?.calls, 2);
	assert.equal(state.tools.get("ls")?.degraded, 1);

	onToolExecutionStart(state, "grep-cut", "ffgrep");
	onToolExecutionEnd(state, "grep-cut", { details: { truncated: true, skipped: 1, matches: [] } }, false);
	assert.equal(state.tools.get("ffgrep")?.truncated, 1);
});
