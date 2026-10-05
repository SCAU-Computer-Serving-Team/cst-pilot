const PREVIEW_ROWS = 6;

export function modelListRows(ids: string[], unavailable: string[], expanded: boolean) {
	const total = ids.length + unavailable.length;
	return {
		ids: expanded ? ids : ids.slice(0, PREVIEW_ROWS),
		unavailable: expanded ? unavailable : unavailable.slice(0, Math.max(0, PREVIEW_ROWS - ids.length)),
		total,
		hasMore: total > PREVIEW_ROWS,
	};
}
