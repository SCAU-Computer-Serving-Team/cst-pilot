import assert from "node:assert/strict";
import { test } from "node:test";
import { backgroundBands, bandOpacity, exitEnvelope, regionFadeMask, regionOpacity } from "./region-fade.ts";

test("400ms退场的相邻帧透明度变化连续，区域间不产生明显阶梯", () => {
	for (const radius of [0, 0.1, 0.25, 0.5, 0.75, 0.95]) {
		let previous = 1;
		for (let i = 1; i <= 24; i++) {
			const progress = i / 24;
			const alpha = regionOpacity(radius, 9.96 + progress * 0.06912, 10, progress, 1336);
			assert.ok(previous - alpha < 0.13, `相邻帧变化过大：${radius}/${progress}/${previous - alpha}`);
			assert.ok(alpha <= previous + 1e-7);
			previous = alpha;
		}
	}
	for (let radius = 0.01; radius < 0.99; radius += 0.01) {
		assert.ok(
			Math.abs(regionOpacity(radius, 10.01, 11, 0.6, 1336) - regionOpacity(radius + 0.005, 10.01, 11, 0.6, 1336)) <
				0.035,
			"空间透明度变化平滑",
		);
	}
});

test("分区边缘沿用背景色带，相邻区域连续覆盖中心到外侧", () => {
	for (const travel of [9.96, 10, 10.2, 15.9]) {
		const bands = backgroundBands(travel);
		assert.equal(bands[0].inner, 0);
		assert.equal(bands.at(-1)!.outer, 1);
		for (let i = 1; i < bands.length; i++) assert.equal(bands[i].inner, bands[i - 1].outer);
	}
	assert.notEqual(backgroundBands(9.96)[0].outer, backgroundBands(10.2)[0].outer);
});

test("中心先淡出，外侧后淡出；区域之间适度重叠", () => {
	assert.equal(bandOpacity(0, 0), 1);
	assert.equal(bandOpacity(0, 0.55), 0);
	assert.equal(bandOpacity(11, 0.3), 1);
	assert.ok(bandOpacity(11, 0.9) > 0 && bandOpacity(11, 0.9) < 1);
	assert.equal(bandOpacity(11, 1), 0);
});

test("色带跨过时间边界后，已经淡出的像素不会重新显露", () => {
	for (const start of [9.75, 9.96, 10, 10.95]) {
		const origin = Math.ceil(start);
		for (const radius of [0, 0.02, 0.1, 0.25, 0.5, 0.75, 0.95]) {
			let previous = 1;
			for (let i = 0; i <= 100; i++) {
				const progress = i / 100;
				const alpha = regionOpacity(radius, start + progress * 0.25056, origin, progress, 1336);
				assert.ok(alpha <= previous + 1e-7, `${start}/${radius}/${progress}: ${previous} → ${alpha}`);
				previous = alpha;
			}
			assert.ok(previous < 1e-7);
		}
	}
});

test("相邻色带的透明度边缘连续，并且在边缘内平滑过渡", () => {
	const travel = 9.96,
		origin = 10,
		progress = 0.45,
		width = 1336;
	const edge = backgroundBands(travel)[4].inner;
	const left = regionOpacity(edge - 1e-7, travel, origin, progress, width);
	const right = regionOpacity(edge + 1e-7, travel, origin, progress, width);
	assert.ok(Math.abs(left - right) < 0.0001);
	assert.ok(
		left > exitEnvelope(edge, progress) * 0.8 + bandOpacity(3, progress) * 0.2 &&
			left < exitEnvelope(edge, progress) * 0.8 + bandOpacity(4, progress) * 0.2,
	);
	assert.equal(bandOpacity(12, 1), 0);
});

test("透明度柔化限制在色带边缘两侧各2px，区域内部保持清晰", () => {
	const travel = 9.96,
		origin = 10,
		progress = 0.45,
		width = 1336;
	const edge = backgroundBands(travel)[4].inner;
	assert.equal(
		regionOpacity(edge - 5 / width, travel, origin, progress, width),
		exitEnvelope(edge - 5 / width, progress) * 0.8 + bandOpacity(3, progress) * 0.2,
	);
	assert.equal(
		regionOpacity(edge + 5 / width, travel, origin, progress, width),
		exitEnvelope(edge + 5 / width, progress) * 0.8 + bandOpacity(4, progress) * 0.2,
	);
});

test("降级遮罩柔化同一套色带边缘，左右对称，不引入位移", () => {
	const mask = regionFadeMask(9.96, 10, 0.4, 1336);
	assert.ok(mask.startsWith("linear-gradient(to right,"));
	assert.ok(mask.includes("0.00000"));
	for (const x of [0.05, 0.2, 0.4]) {
		assert.ok(
			Math.abs(
				regionOpacity(Math.abs(x - 0.5) * 2, 9.96, 10, 0.4, 1336) -
					regionOpacity(Math.abs(1 - x - 0.5) * 2, 9.96, 10, 0.4, 1336),
			) < 1e-12,
		);
	}
});
