import assert from "node:assert/strict";
import { test } from "node:test";
import { newState, onToolExecutionEnd, onToolExecutionStart } from "../collect.ts";

test("磁盘遍历仅在结果不完整时计为降级", () => {
	const state = newState("fixture", "new", "web");
	for (const [id, usage] of Object.entries({
		complete: { method: "node-walk", truncated: false },
		budget: { method: "node-walk", truncated: true },
		denied: { method: "node-walk", denied: ["fixture"] },
	})) {
		onToolExecutionStart(state, id, "disk");
		onToolExecutionEnd(state, id, { details: { usage } }, false);
	}
	assert.equal(state.tools.get("disk")?.calls, 3);
	assert.equal(state.tools.get("disk")?.degraded, 2);
});
