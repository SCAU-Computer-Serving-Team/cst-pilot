export const DEFAULT_CONTEXT_WINDOW = 256 * 1024;
export const FALLBACK_MAX_TOKENS = 16384;

/** 同频时取较小输出上限，保证目录顺序变化不影响结果。 */
export function commonOutputLimit(models: { maxTokens?: number }[]): number {
	const counts = new Map<number, number>();
	for (const { maxTokens } of models) {
		if (typeof maxTokens === "number" && Number.isSafeInteger(maxTokens) && maxTokens > 0)
			counts.set(maxTokens, (counts.get(maxTokens) ?? 0) + 1);
	}
	return [...counts.entries()].sort((a, b) => b[1] - a[1] || a[0] - b[0])[0]?.[0] ?? FALLBACK_MAX_TOKENS;
}
