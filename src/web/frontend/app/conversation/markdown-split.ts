// 流式 Markdown 的分段渲染：把正在生成的正文切成「已定稿前缀 + 流式尾段」。
// 前缀用 memo 缓存跳过重解析，每帧只解析尾段；结束后整体单块渲染，视觉还原。
// 只有在分割点两侧都不会改变 Markdown 块语义时才切分，否则整段作为尾段。

/** 行首为列表、引用、表格、缩进续行等块级延续形态。 */
const CONTINUATION = /^(?:[-*+]\s|\d+[.)]\s|>|\|| {4,}|\t)/;

/** 段落内的围栏代码块起点（``` 或 ~~~），用于判断未闭合代码块。 */
const fenceCount = (part: string) => (part.match(/^[ \t]{0,3}(?:```|~~~)/gm) ?? []).length;

const isBlank = (part: string) => !part.trim();

export function splitStableText(text: string): { stable: string; tail: string } {
	if (!text.includes("\n\n")) return { stable: "", tail: text };
	const parts = text.split("\n\n");
	// 尾段必须自包含（围栏配平）：未闭合的代码块整体留在尾段。
	let tailStart = parts.length - 1;
	let fences = 0;
	while (tailStart > 0) {
		fences += fenceCount(parts[tailStart]!);
		if (fences % 2 === 0) break;
		tailStart--;
	}
	if (tailStart === 0) return { stable: "", tail: text };
	// 尾段以块级延续形态开头（列表项、表格、引用、缩进）时不切：
	// 它可能属于前缀末尾的同一个块（如有序列表的后续项），切开会改变语义。
	const tail = parts.slice(tailStart).join("\n\n");
	const tailFirst = parts[tailStart]!;
	if (!isBlank(tailFirst) && CONTINUATION.test(tailFirst)) return { stable: "", tail: text };
	// 前缀末段是完整闭合的块（列表/表格/引用/代码块）：后续新段与它分属不同块，切开安全。
	const prefix = parts.slice(0, tailStart).join("\n\n");
	return { stable: prefix, tail };
}
