import type { ContentPart, Message } from "../app/web-state";
import type { CardView } from "./map-tool";

type Call = Extract<ContentPart, { type: "toolCall" }>;
export type ToolState = CardView["status"] | "running" | "interrupted";

export function toolState(result: Message | undefined, status: CardView["status"] | undefined, live = false): ToolState {
  if (!result) return live ? "running" : "interrupted";
  return result.isError ? "error" : status ?? "success";
}

export function isStandaloneTool(call: Call, result?: Message): boolean {
  return call.name === "web_search" || call.name === "runbook"
    || (call.name === "sys" && (call.arguments.scope ?? "overview") === "overview")
    || (call.name === "read" && Array.isArray(result?.content) && result.content.some((part) => part.type === "image"));
}
