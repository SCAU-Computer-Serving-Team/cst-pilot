/** null 表示默认范围，[] 表示 Web 端明确清空；全部选中归一为 null。 */
export function normalizeScope(ids: string[], all: string[]): string[] | null {
	const selected = [...new Set(ids.filter((id) => all.includes(id)))];
	return selected.length > 0 && selected.length === all.length && all.every((id) => selected.includes(id))
		? null
		: selected;
}
export function toggleModel(selection: string[] | null, all: string[], id: string) {
	const current = selection ?? all;
	return normalizeScope(current.includes(id) ? current.filter((item) => item !== id) : [...current, id], all);
}
export function moveModel(selection: string[] | null, id: string, target: string) {
	if (!selection || id === target || !selection.includes(id) || !selection.includes(target)) return selection;
	const next = selection.filter((item) => item !== id);
	next.splice(selection.indexOf(target), 0, id);
	return next;
}
