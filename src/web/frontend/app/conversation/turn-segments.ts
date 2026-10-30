import { isStandaloneTool } from "../cards/tool-kind.ts";
import type { Message } from "../data/web-state";
import type { TurnPart } from "./conversation-parts";
export type TurnSegment = { kind: "process" | "visible"; parts: TurnPart[]; key: string };

function ordinaryResults(parts: TurnPart[], results: Map<string, Map<string, Message>>): (Message | undefined)[] {
	const calls: (Message | undefined)[] = [];
	for (const { entry, part } of parts) {
		if (part.kind !== "tools") continue;
		const entryResults = results.get(entry.id ?? `stamp:${entry.message.timestamp}`);
		for (const call of part.calls) {
			const result = entryResults?.get(call.id);
			if (!isStandaloneTool(call, result)) calls.push(result);
		}
	}
	return calls;
}

/** 只有实际普通调用才显示调用数；思考、正文与独立图片保留各自内容入口。 */
export function processSegmentLabel(
	parts: TurnPart[],
	results: Map<string, Map<string, Message>>,
	live: boolean,
): string | undefined {
	const calls = ordinaryResults(parts, results);
	const completed = calls.filter((result) => result !== undefined).length;
	if (calls.length) {
		if (!completed) return live ? `正在调用 ${calls.length} 个工具` : "检查未完成";
		return completed === calls.length
			? `已完成 ${completed} 次工具调用`
			: `已完成 ${completed} / ${calls.length} 次工具调用`;
	}
	const meaningful = parts.filter(({ part }) =>
		part.kind === "thinking"
			? part.part.redacted || part.part.thinking.trim()
			: part.kind === "text"
				? part.text.trim()
				: part.kind === "tools" && part.calls.length,
	);
	if (!meaningful.length) return undefined;
	return meaningful.every(({ part }) => part.kind === "thinking") ? "思考过程" : "处理过程";
}
/** 特殊组件持续挂载；相邻过程独立折叠，始终保留第一段工作时长入口。 */
export function turnSegments(process: TurnPart[]): TurnSegment[] {
	const segments: TurnSegment[] = [];
	for (const item of process) {
		const kind = item.part.kind === "special" ? "visible" : "process";
		const previous = segments.at(-1);
		if (previous?.kind === kind) previous.parts.push(item);
		else
			segments.push({
				kind,
				parts: [item],
				key: `${kind}-${item.entry.message.timestamp}-${segments.length}`,
			});
	}
	if (!segments.length || segments[0]?.kind !== "process")
		segments.unshift({ kind: "process", parts: [], key: "process-start" });
	segments[0]!.key = "process-start";
	return segments;
}
