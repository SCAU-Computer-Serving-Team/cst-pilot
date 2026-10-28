import assert from "node:assert/strict";
import { test } from "node:test";
import { glyphLightFrames, lightGlyphs } from "./running-light-timing.ts";

test("同一时刻按字位形成中心、相邻、隔一字三档，范围以外为零", () => {
	const count = 11,
		center = 5,
		offset = (center + 3) / (count + 5);
	const strengths = Array.from({ length: count }, (_, index) => {
		const frames = glyphLightFrames(index, count);
		const exact = frames.find((frame) => frame.offset === offset);
		return exact ? exact["--running-glyph-strength"] : 0;
	});
	assert.deepEqual(strengths, [0, 0, 0, 0.08, 0.24, 0.48, 0.24, 0.08, 0, 0, 0]);
	for (let index = 0; index < count; index++) {
		const frames = glyphLightFrames(index, count);
		assert.equal(frames[0].offset, 0);
		assert.equal(frames.at(-1)?.offset, 1);
		assert.ok(frames.every((frame) => frame.offset! >= 0 && frame.offset! <= 1));
		assert.ok(frames.every((frame, i) => i === 0 || frame.offset! >= frames[i - 1].offset!));
	}
});

test("逐字分割不拆开组合字符和 Emoji，短状态也可完整扫过", () => {
	assert.deepEqual(lightGlyphs("已思考 e\u0301 👨‍👩‍👧‍👦"), ["已", "思", "考", " ", "e\u0301", " ", "👨‍👩‍👧‍👦"]);
	assert.deepEqual(lightGlyphs(""), []);
	assert.ok(
		glyphLightFrames(0, 1).some((frame) => frame["--running-glyph-strength"] === 0.48 && frame.offset === 0.5),
	);
});
