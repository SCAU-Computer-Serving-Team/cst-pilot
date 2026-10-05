import assert from "node:assert/strict";
import { test } from "node:test";
import { acceptsOAuthStatus } from "./oauth-state.ts";

test("发起新授权前，旧成功状态不能触发跳转", () => {
	assert.equal(acceptsOAuthStatus({ state: "idle" }, { state: "succeeded", flowId: "old" }, ""), false);
	assert.equal(acceptsOAuthStatus({ state: "idle" }, { state: "succeeded", flowId: "old" }, "new"), false);
});
test("迟到的状态不能恢复旧输入步骤，取消后不能回到等待态", () => {
	assert.equal(
		acceptsOAuthStatus(
			{ state: "pending", flowId: "flow", revision: 4 },
			{ state: "pending", flowId: "flow", revision: 2 },
			"flow",
		),
		false,
	);
	assert.equal(
		acceptsOAuthStatus(
			{ state: "cancelled", flowId: "flow", revision: 4 },
			{ state: "pending", flowId: "flow", revision: 5 },
			"flow",
		),
		false,
	);
	assert.equal(
		acceptsOAuthStatus(
			{ state: "pending", flowId: "flow", revision: 4 },
			{ state: "succeeded", flowId: "flow", revision: 5 },
			"flow",
		),
		true,
	);
});
