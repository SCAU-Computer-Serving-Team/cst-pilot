import { isStandaloneTool } from "../cards/tool-state.ts";
import type { Message } from "../data/web-state";
import type { TurnPart } from "./conversation-parts";
export type TurnSegment = { kind: "process" | "visible"; parts: TurnPart[]; key: string };

export function completedToolCalls(parts: TurnPart[], results: Map<string, Map<string, Message>>): number {
	let count = 0;
	for (const { entry, part } of parts) {
		if (part.kind !== "tools") continue;
		const entryResults = results.get(entry.id ?? `stamp:${entry.message.timestamp}`);
		for (const call of part.calls) {
			const result = entryResults?.get(call.id);
			if (result && !isStandaloneTool(call, result)) count++;
		}
	}
	return count;
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
