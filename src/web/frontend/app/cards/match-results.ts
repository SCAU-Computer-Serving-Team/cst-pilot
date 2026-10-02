import type { Message } from "../app/web-state";

type Entry = { id?: string; message: Message };

/** Tool-call IDs identify a call within a turn; they can be reused in later turns. */
export function matchToolResults(entries: Entry[]): Map<string, Map<string, Message>> {
	const groups = new Map<string, Map<string, Message>>();
	const pending = new Map<string, string[]>();
	for (const { id, message } of entries) {
		if (message.role === "assistant" && Array.isArray(message.content)) {
			const key = id ?? `stamp:${message.timestamp}`;
			for (const call of message.content) {
				if (call.type !== "toolCall") continue;
				if (!groups.has(key)) groups.set(key, new Map());
				pending.set(call.id, [...(pending.get(call.id) ?? []), key]);
			}
		} else if (message.role === "toolResult" && message.toolCallId) {
			const queue = pending.get(message.toolCallId);
			const key = queue?.shift();
			if (key) groups.get(key)?.set(message.toolCallId, message);
		}
	}
	return groups;
}
