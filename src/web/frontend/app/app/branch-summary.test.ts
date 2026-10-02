import assert from "node:assert/strict";
import { test } from "node:test";
import { branchSummaryChoice, branchSummaryHref, summaryBody } from "./branch-summary.ts";

test("待办参数要同时有条目与已知模式", () => {
	assert.deepEqual(branchSummaryChoice(new URLSearchParams("branch=a1&mode=custom")), {
		entryId: "a1",
		mode: "custom",
	});
	assert.deepEqual(branchSummaryChoice(new URLSearchParams("branch=a1&mode=summarize")), {
		entryId: "a1",
		mode: "summarize",
	});
	assert.equal(branchSummaryChoice(new URLSearchParams("branch=a1")), undefined);
	assert.equal(branchSummaryChoice(new URLSearchParams("mode=custom")), undefined);
	assert.equal(branchSummaryChoice(new URLSearchParams("branch=a1&mode=none")), undefined);
	assert.equal(branchSummaryChoice(new URLSearchParams("preview=1")), undefined);
});

test("跳转地址带上条目与模式，特殊字符转义", () => {
	assert.equal(branchSummaryHref("s 1", "a&b", "summarize"), "/s/s%201?branch=a%26b&mode=summarize");
});

test("请求体始终带 summarize，提示词可选", () => {
	assert.deepEqual(summaryBody("a1"), { entryId: "a1", summarize: true });
	assert.deepEqual(summaryBody("a1", "保留报错原文"), {
		entryId: "a1",
		summarize: true,
		customInstructions: "保留报错原文",
	});
});
