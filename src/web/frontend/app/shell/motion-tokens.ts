/** 解析 CSS 时间值字符串（ms / s）为毫秒；遇到未求值表达式或非法值返回 null。 */
export function parseCssTimeMs(raw: string): number | null {
	const value = raw.trim().toLowerCase();
	if (!value) return null;
	const match = /^([+-]?(?:\d+\.?\d*|\.\d+))(ms|s)$/.exec(value);
	if (!match) return null;
	const amount = Number.parseFloat(match[1]);
	if (!Number.isFinite(amount) || amount < 0) return null;
	return match[2] === "ms" ? amount : amount * 1000;
}

/** 安全读取动效时长令牌（毫秒），防止未支持 @property 的环境传入 NaN 导致 WAAPI 抛错。 */
export function readDurationMs(element: Element, token: string, fallbackMs: number): number {
	const raw = getComputedStyle(element).getPropertyValue(token);
	return parseCssTimeMs(raw) ?? fallbackMs;
}

/** 安全读取动效缓动曲线令牌。 */
export function readEasing(
	element: Element,
	token = "--ease-smooth-out",
	fallback = "cubic-bezier(0.22, 1, 0.36, 1)",
): string {
	return getComputedStyle(element).getPropertyValue(token).trim() || fallback;
}
