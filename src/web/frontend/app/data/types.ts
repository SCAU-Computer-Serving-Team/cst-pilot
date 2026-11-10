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
	clientMessageId?: string;
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

export type ExportOptions = {
	format: "png" | "html";
	theme: "light" | "dark";
	includeThinking: boolean;
	includeTools: boolean;
};

export type ExportSnapshot = {
	sessionId: string;
	title: string;
	capturedAt: string;
	options: ExportOptions & { date: string };
	entries: ConversationEntry[];
};

export type ExportDocumentSource = { body: string; styles: { css: string; base: string }[] };

export type ExportFile = { name: string; bytes: number; url: string };

export type SessionExport = {
	id: string;
	sessionId: string;
	title: string;
	capturedAt: string;
	options: ExportSnapshot["options"];
	status: "queued" | "rendering" | "completed" | "failed";
	files: ExportFile[];
	directory: string;
	error?: { code: string; message: string };
};
