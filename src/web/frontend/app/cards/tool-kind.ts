import type { ContentPart, Message } from "../data/web-state";

export type SpecialToolCall = Extract<ContentPart, { type: "toolCall" }>;
export type SpecialToolKind = "sys-overview" | "runbook" | "web-search" | "bitlocker";

/** 特殊组件的唯一分类入口；过程分段、工具分组和计数共用。 */
export function specialToolKind(call: SpecialToolCall): SpecialToolKind | undefined {
	if (call.name === "sys" && (call.arguments.scope ?? "overview") === "overview") return "sys-overview";
	if (["runbook", "command_plan", "runbook_deliver"].includes(call.name)) return "runbook";
	if (call.name === "web_search") return "web-search";
	if (call.name === "bitlocker" && (call.arguments.scope === "unlock" || call.arguments.scope === "decrypt"))
		return "bitlocker";
	return undefined;
}

export function isStandaloneTool(call: SpecialToolCall, result?: Message): boolean {
	return (
		specialToolKind(call) !== undefined ||
		(call.name === "read" && Array.isArray(result?.content) && result.content.some((part) => part.type === "image"))
	);
}
