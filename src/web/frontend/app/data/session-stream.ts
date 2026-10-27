import { playableText } from "./stream-player.ts";
import type { Message, SessionDetail } from "./web-state";

/** message_end 按角色和时间承接完整消息，避免等待 HTTP 快照期间清空或错配轮次。 */
export function commitMessage(state: SessionDetail, message: Message): SessionDetail {
	const same = (candidate: Message) =>
		candidate.role === message.role &&
		candidate.timestamp === message.timestamp &&
		candidate.toolCallId === message.toolCallId;
	const messages = state.messages.some(same)
		? state.messages.map((item) => (same(item) ? message : item))
		: [...state.messages, message];
	const entries = state.entries.some((item) => same(item.message))
		? state.entries.map((item) => (same(item.message) ? { ...item, message } : item))
		: [...state.entries, { id: `stream-${message.role}-${message.timestamp}-${message.toolCallId ?? ""}`, message }];
	return { ...state, messages, entries };
}

export function containsAssistant(state: SessionDetail, message: Message): boolean {
	return state.entries.some(
		({ message: item }) =>
			item.role === "assistant" &&
			item.timestamp === message.timestamp &&
			playableText(item).startsWith(playableText(message)),
	);
}
