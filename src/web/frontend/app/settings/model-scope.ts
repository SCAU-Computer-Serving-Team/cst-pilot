function isSubsequenceOrder(ids: string[], all: string[]): boolean {
	let cursor = -1;
	for (const id of ids) {
		const pos = all.indexOf(id);
		if (pos <= cursor) return false;
		cursor = pos;
	}
	return true;
}

/** null 表示默认范围及默认顺序，[] 表示 Web 端明确清空；仅默认顺序全选归一为 null，显式重排顺序保留。 */
export function normalizeScope(ids: string[], all: string[]): string[] | null {
	const selected = [...new Set(ids.filter((id) => all.includes(id)))];
	return selected.length > 0 && selected.length === all.length && all.every((id, index) => selected[index] === id)
		? null
		: selected;
}
export function toggleModel(selection: string[] | null, all: string[], id: string) {
	const current = selection ?? all;
	if (current.includes(id)) {
		return normalizeScope(
			current.filter((item) => item !== id),
			all,
		);
	}
	if (isSubsequenceOrder(current, all)) {
		const nextSet = new Set([...current, id]);
		return normalizeScope(
			all.filter((item) => nextSet.has(item)),
			all,
		);
	}
	return normalizeScope([...current, id], all);
}
export function moveModel(selection: string[] | null, id: string, target: string) {
	if (!selection || id === target || !selection.includes(id) || !selection.includes(target)) return selection;
	const next = selection.filter((item) => item !== id);
	next.splice(selection.indexOf(target), 0, id);
	return next;
}
