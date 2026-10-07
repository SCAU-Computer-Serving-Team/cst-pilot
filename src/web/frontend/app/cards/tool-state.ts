import type { ContentPart, Message } from "../data/web-state";
import type { CardView } from "./map-tool";

type Call = Extract<ContentPart, { type: "toolCall" }>;
export type ToolState = CardView["status"] | "pending" | "running" | "interrupted";

export function toolState(
	result: Message | undefined,
	status: CardView["status"] | undefined,
	live = false,
	started = true,
): ToolState {
	if (!result) return live ? (started ? "running" : "pending") : "interrupted";
	return result.isError ? "error" : (status ?? "success");
}

export function splitToolCalls(
	calls: Call[],
	results: Map<string, Message>,
): { kind: "group" | "standalone"; calls: Call[] }[] {
	const segments: { kind: "group" | "standalone"; calls: Call[] }[] = [];
	for (const call of calls) {
		const kind = isStandaloneTool(call, results.get(call.id)) ? "standalone" : "group";
		const last = segments.at(-1);
		if (kind === "group" && last?.kind === "group") last.calls.push(call);
		else segments.push({ kind, calls: [call] });
	}
	return segments;
}

export function isStandaloneTool(call: Call, result?: Message): boolean {
	return (
		call.name === "web_search" ||
		call.name === "runbook" ||
		(call.name === "sys" && (call.arguments.scope ?? "overview") === "overview") ||
		(call.name === "read" && Array.isArray(result?.content) && result.content.some((part) => part.type === "image"))
	);
}
