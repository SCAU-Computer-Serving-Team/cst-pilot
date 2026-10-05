import assert from "node:assert/strict";
import { test } from "node:test";
import { FrameClock } from "./frame-clock.ts";

for (const rate of [30, 60, 120, 144, 160, 240, 360]) {
	test(`${rate}Hz 的背景在一秒后推进相同的时间`, () => {
		const clock = new FrameClock(0);
		let time = 0;
		for (let frame = 1; frame <= rate; frame++) time = clock.step((frame * 1000) / rate);
		assert.ok(Math.abs(time - 1) < 1e-10);
	});
}
test("绘制延迟与不均匀帧间隔不会改变背景速度", () => {
	const clock = new FrameClock(0);
	let elapsed = 0;
	for (const now of [8, 24, 61, 180, 500, 1000]) elapsed = clock.step(now);
	assert.equal(elapsed, 1);
});

test("暂停、恢复和减弱动态不补播或倒退", () => {
	const clock = new FrameClock(0);
	assert.equal(clock.step(10), 0.01);
	clock.resume(10_000);
	assert.equal(clock.step(10_010), 0.02);
	assert.equal(clock.step(10_020, true), 0.02);
	assert.equal(clock.step(10_030), 0.03);
	assert.equal(clock.step(10_000), 0.03);
});
