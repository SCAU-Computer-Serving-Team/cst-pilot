import assert from "node:assert/strict";
import { test } from "node:test";
import { presentError } from "./error-presenter.ts";

test("认证错误提供重新登录且保留技术详情", () => {
	const e = presentError('401 {"error":{"type":"authentication_error","message":"invalid api key"}}');
	assert.equal(e.kind, "auth");
	assert.match(e.title, /重新登录/);
	assert.equal(e.action, "重新登录");
	assert.match(e.detail, /401/);
});
test("写失败、限流与网络错误分类，未知错误不伪造原因", () => {
	assert.equal(presentError("工具包目录无法写入，输入未被接受").kind, "storage");
	assert.equal(presentError("429 rate limit").kind, "rate");
	assert.equal(presentError("Failed to fetch").kind, "connection");
	const e = presentError("provider internal exception");
	assert.equal(e.kind, "general");
	assert.equal(e.detail, "provider internal exception");
});
test("详情遮蔽密钥，不将技术文本放主提示", () => {
	const e = presentError("500 apiKey=private-placeholder-token Authorization: Bearer example-secret");
	assert.ok(!e.title.includes("500"));
	assert.ok(!e.detail.includes("private-placeholder-token"));
	assert.ok(!e.detail.includes("example-secret"));
});
