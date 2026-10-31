import { ExportError } from "./snapshot.ts";

export type ExportPage = { top: number; height: number };

/** 按段落、表格与代码行选取切点；输出连续且无重叠的完整高度。 */
export function exportPages(height: number, candidates: readonly number[]): ExportPage[] {
	if (!Number.isSafeInteger(height) || height <= 0)
		throw new ExportError(400, "export_layout_invalid", "导出文档没有有效高度");
	if (height > 100_000)
		throw new ExportError(413, "export_too_tall", "图片超过导出高度限制，请缩短当前分支或改用 HTML。");
	const cuts = candidates
		.filter((value) => Number.isSafeInteger(value) && value > 0 && value < height)
		.sort((a, b) => a - b);
	const pages: ExportPage[] = [];
	let top = 0,
		index = 0;
	while (top < height) {
		const target = Math.min(top + 3200, height);
		let cut = target;
		if (target < height)
			while (index < cuts.length && cuts[index] <= target) {
				if (cuts[index] >= top + 1600) cut = cuts[index];
				index++;
			}
		pages.push({ top, height: cut - top });
		top = cut;
	}
	return pages;
}
