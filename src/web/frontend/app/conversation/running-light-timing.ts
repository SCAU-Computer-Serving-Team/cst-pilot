/** 五字光照：中心最强，左右相邻次之，隔一字最弱；其余保持原色。 */
export function glyphLightFrames(index: number, count: number): Keyframe[] {
	const cycle = count + 5;
	const weights = [0, 0.08, 0.24, 0.48, 0.24, 0.08, 0];
	return [
		{ offset: 0, "--running-glyph-strength": 0 },
		...weights.map((strength, step) => ({ offset: (index + step) / cycle, "--running-glyph-strength": strength })),
		{ offset: 1, "--running-glyph-strength": 0 },
	];
}

const segmenter = new Intl.Segmenter(undefined, { granularity: "grapheme" });
/** 字素保持组合字符、Emoji 与变体选择符完整；空白也占一个字位。 */
export function lightGlyphs(text: string): string[] {
	return Array.from(segmenter.segment(text), (part) => part.segment);
}
