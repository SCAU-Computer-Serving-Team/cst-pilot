import type { ContentPart, QueueItem } from "./web-state";

type Thinking = Extract<ContentPart, { type: "thinking" }>;
type Call = Extract<ContentPart, { type: "toolCall" }>;
export type ConversationPart = { kind: "thinking"; part: Thinking } | { kind: "text"; text: string } | { kind: "tools"; calls: Call[] };

/** The model can put prose before and after a tool call in the same assistant message. */
export function orderedParts(content: ContentPart[]): ConversationPart[] {
  const parts: ConversationPart[] = [];
  for (const part of content) {
    const last = parts.at(-1);
    if (part.type === "thinking") parts.push({ kind: "thinking", part });
    else if (part.type === "toolCall") {
      if (last?.kind === "tools") last.calls.push(part);
      else parts.push({ kind: "tools", calls: [part] });
    } else if (part.type === "text") {
      if (last?.kind === "text") last.text += `\n${part.text}`;
      else parts.push({ kind: "text", text: part.text });
    }
  }
  return parts;
}

/** The server classifies each accepted input; never infer queue membership from timing. */
export function visibleQueueItems(items: QueueItem[]): QueueItem[] {
  return items.filter((item) => item.status === "failed" || item.status === "pending" && item.delivery !== "direct");
}
