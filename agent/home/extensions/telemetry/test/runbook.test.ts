import assert from "node:assert/strict";
import { test } from "node:test";
import { newState, onToolExecutionEnd, onToolExecutionStart } from "../collect.ts";

test("命令方案与交付分开统计，条目数只取交付结果且不记录命令正文", () => {
	const state = newState("fixture", "new", "web");
	for (const name of ["command_plan", "runbook_deliver", "runbook"]) {
		onToolExecutionStart(state, name, name);
		onToolExecutionEnd(
			state,
			name,
			{
				content: [],
				details: {
					runbook: { items: 2, commands: [{ command: "private command" }, { command: "private command" }] },
				},
			},
			false,
		);
	}
	assert.equal(state.tools.get("command_plan")?.entriesTotal, undefined);
	assert.equal(state.tools.get("runbook_deliver")?.entriesTotal, 2);
	assert.equal(state.tools.get("runbook")?.entriesTotal, 2);
	assert.equal(JSON.stringify([...state.tools.values()]).includes("private command"), false);
});
