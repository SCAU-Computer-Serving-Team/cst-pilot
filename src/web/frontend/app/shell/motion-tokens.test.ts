import assert from "node:assert/strict";
import { test } from "node:test";
import { parseCssTimeMs } from "./motion-tokens.ts";

test("parseCssTimeMs 正确解析 ms 与 s 并拒绝未求值 calc 表达式", () => {
	assert.equal(parseCssTimeMs("150ms"), 150);
	assert.equal(parseCssTimeMs("0.34s"), 340);
	assert.equal(parseCssTimeMs(".24s"), 240);
	assert.equal(parseCssTimeMs("0s"), 0);
	assert.equal(parseCssTimeMs("0ms"), 0);
	assert.equal(parseCssTimeMs("calc(340ms * 1)"), null);
	assert.equal(parseCssTimeMs("calc(.34s * var(--motion-scale))"), null);
	assert.equal(parseCssTimeMs("-10ms"), null);
	assert.equal(parseCssTimeMs(""), null);
});
