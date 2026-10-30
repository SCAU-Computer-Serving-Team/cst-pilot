export type ContentPart =
	| { type: "text"; text: string }
	| { type: "thinking"; thinking: string; redacted?: boolean }
	| { type: "image"; mimeType: string; data: string }
	| {
			type: "toolCall";
			id: string;
			name: string;
			arguments: Record<string, unknown>;
			execution?: { startedAt: number; endedAt?: number; stage?: string; result?: Message };
	  };
export type Message = {
	role: "user" | "assistant" | "toolResult" | "branchSummary";
	content: string | ContentPart[];
	timestamp: number;
	work?: { startedAt: number; endedAt: number };
	summary?: string;
	fromId?: string;
	toolCallId?: string;
	toolName?: string;
	isError?: boolean;
	details?: unknown;
	stopReason?: string;
	errorMessage?: string;
};
export type ConversationEntry = { id: string; parentId?: string | null; message: Message };
