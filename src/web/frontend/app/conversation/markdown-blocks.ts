/**
 * 将流式 Markdown 拆分为「已定稿的完整块」与「末尾活跃流式块」：
 * 仅在围栏代码块外部遇到空行（\n\n）时切分，确保已读块稳定复用 DOM，不重播淡入、不丢文本选区。
 */
export function splitStreamingMarkdownBlocks(text: string): { settled: string[]; tail: string } {
	if (!text) return { settled: [], tail: "" };
	const lines = text.split("\n");
	const blocks: string[] = [];
	let current: string[] = [];
	let fenceChar: "`" | "~" | null = null;
	let fenceLen = 0;

	for (let i = 0; i < lines.length; i++) {
		const line = lines[i];
		const trimmed = line.trimStart();
		const fenceMatch = /^(`{3,}|~{3,})/.exec(trimmed);
		if (fenceMatch) {
			const marker = fenceMatch[1];
			const ch = marker[0] as "`" | "~";
			if (!fenceChar) {
				fenceChar = ch;
				fenceLen = marker.length;
			} else if (fenceChar === ch && marker.length >= fenceLen && trimmed.slice(marker.length).trim() === "") {
				fenceChar = null;
				fenceLen = 0;
			}
		}

		if (!fenceChar && line.trim() === "" && current.length > 0) {
			blocks.push(current.join("\n"));
			current = [];
			continue;
		}
		current.push(line);
	}

	const tail = current.join("\n");
	if (tail === "" && blocks.length > 0) {
		const last = blocks.pop() ?? "";
		return { settled: blocks, tail: last };
	}
	return { settled: blocks, tail };
}
