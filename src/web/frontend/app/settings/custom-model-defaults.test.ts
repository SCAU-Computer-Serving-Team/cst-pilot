import assert from "node:assert/strict";
import { test } from "node:test";
import { commonOutputLimit, DEFAULT_CONTEXT_WINDOW, FALLBACK_MAX_TOKENS } from "./custom-model-defaults.ts";

test("自定义模型默认 256K 上下文，最大输出取模型列表众数", () => {
	assert.equal(DEFAULT_CONTEXT_WINDOW, 262144);
	assert.equal(commonOutputLimit([{ maxTokens: 32768 }, { maxTokens: 8192 }, { maxTokens: 32768 }]), 32768);
});
test("忽略无效输出上限，同频取较小值，空目录使用回退值", () => {
	assert.equal(commonOutputLimit([{ maxTokens: 0 }, {}, { maxTokens: NaN }, { maxTokens: -1 }]), FALLBACK_MAX_TOKENS);
	assert.equal(commonOutputLimit([{ maxTokens: 16384 }, { maxTokens: 8192 }]), 8192);
	assert.equal(commonOutputLimit([{ maxTokens: 8192 }, { maxTokens: 16384 }]), 8192);
});
