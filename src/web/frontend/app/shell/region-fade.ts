const clamp = (value: number) => Math.max(0, Math.min(1, value));
const smoothstep = (value: number) => {
	const t = clamp(value);
	return t * t * (3 - 2 * t);
};

/** 与 blue-hour.glsl 的色带边缘一致，距离以主区中心到边缘归一化。 */
export function backgroundBands(travel: number, count = 11) {
	const bands: { lane: number; inner: number; outer: number }[] = [];
	for (let lane = Math.floor(-travel); lane < Math.ceil(count - travel); lane++) {
		bands.push({
			lane,
			inner: Math.max(0, (lane + travel) / count),
			outer: Math.min(1, (lane + 1 + travel) / count),
		});
	}
	return bands;
}

/** 相邻色带的淡出重叠；区域编号固定在退出开始时，边缘继续随背景动画更新。 */
export function bandOpacity(order: number, progress: number, count = 11) {
	const delay = (Math.max(0, Math.min(count, order)) / count) * 0.45;
	return 1 - smoothstep((progress - delay) / 0.55);
}

/** 连续中心向外包络与长重叠色带叠加，减小逐条消失造成的透明度阶梯。 */
export function exitEnvelope(radius: number, progress: number) {
	return 1 - smoothstep((progress - clamp(radius) * 0.45) / 0.55);
}

export function regionOpacity(
	radius: number,
	travel: number,
	origin: number,
	progress: number,
	width: number,
	count = 11,
) {
	const r = Math.min(1 - 1e-7, Math.max(0, radius));
	const lane = Math.floor(r * count - travel);
	const inner = Math.max(0, (lane + travel) / count);
	const outer = Math.min(1, (lane + 1 + travel) / count);
	const order = lane + origin;
	const current = bandOpacity(order, progress, count);
	const feather = Math.min(4 / Math.max(1, width), 0.2 / count);
	let band = current;
	if (inner > 0 && r - inner < feather) {
		const mix = smoothstep((r - inner) / feather);
		band = ((bandOpacity(order - 1, progress, count) + current) / 2) * (1 - mix) + current * mix;
	} else if (outer < 1 && outer - r < feather) {
		const mix = smoothstep((outer - r) / feather);
		band = current * mix + ((current + bandOpacity(order + 1, progress, count)) / 2) * (1 - mix);
	}
	return exitEnvelope(r, progress) * 0.8 + band * 0.2;
}

/** 无 WebGL 时沿同一套分区坐标生成柔化透明遮罩。 */
export function regionFadeMask(travel: number, origin: number, progress: number, width: number) {
	const positions = new Set(Array.from({ length: 65 }, (_, i) => i / 64));
	const feather = 2 / Math.max(1, width);
	for (const band of backgroundBands(travel)) {
		for (const radius of [band.inner, band.outer]) {
			for (const x of [(1 - radius) / 2, (1 + radius) / 2]) {
				for (const offset of [-feather, 0, feather]) positions.add(clamp(x + offset));
			}
		}
	}
	const stops = [...positions]
		.sort((a, b) => a - b)
		.map((x) => {
			const alpha = regionOpacity(Math.abs(x - 0.5) * 2, travel, origin, progress, width);
			return `rgba(0,0,0,${alpha.toFixed(5)}) ${(x * 100).toFixed(5)}%`;
		});
	return `linear-gradient(to right,${stops.join(",")})`;
}
