import assert from "node:assert/strict";
import { test } from "node:test";
import { browserJson } from "../server/session/browser-json.ts";

test("浏览器快照、SSE 历史和工具增量不回显恢复密码，原对象不变", () => {
	const password = Array(8).fill("123456").join("-");
	const value = {
		type: "message_update",
		message: {
			content: [
				{ type: "toolCall", arguments: { recoveryPassword: password } },
				{ type: "text", text: `恢复密码 ${password}` },
			],
		},
		assistantMessageEvent: { type: "toolcall_delta", delta: "123456-" },
	};
	const text = browserJson(value);
	assert.equal(text.includes(password), false);
	assert.equal(text.includes("123456-"), false);
	assert.equal(value.message.content[0]?.arguments?.recoveryPassword, password);
	assert.equal(JSON.parse(text).assistantMessageEvent, undefined);
});
test("部分或无效密码参数也遮蔽，普通数据保留", () => {
	const text = browserJson({ recoveryPassword: "partial", count: 0, active: false });
	assert.equal(text.includes("partial"), false);
	assert.equal(JSON.parse(text).count, 0);
	assert.equal(JSON.parse(text).active, false);
});
