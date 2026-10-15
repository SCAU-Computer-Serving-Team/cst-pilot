import assert from "node:assert/strict";
import { test } from "node:test";
import { newestBitLocker, type PendingBitLocker, requestForCall } from "./bitlocker-state.ts";

const request: PendingBitLocker = {
	requestId: "r",
	toolCallId: "reused",
	entryId: "new-entry",
	kind: "password",
	expiresAt: 1,
	targets: [],
};
test("迟到快照不能恢复已消费的请求", () => {
	const current = { revision: 2, requests: [] };
	assert.equal(newestBitLocker(current, { revision: 1, requests: [request] }), current);
	assert.equal(newestBitLocker(current, { revision: 3, requests: [request] }).requests.length, 1);
});
test("后端重启使用新通道身份，不能保留原请求", () => {
	const current = { revision: 20, epoch: "old", requests: [request] };
	const next = { revision: 0, epoch: "new", requests: [] };
	assert.equal(newestBitLocker(current, next), next);
});
test("调用 ID 复用时仍绑定助手条目", () => {
	assert.equal(requestForCall([request], "reused", "old-entry"), undefined);
	assert.equal(requestForCall([request], "reused", "new-entry"), request);
});
