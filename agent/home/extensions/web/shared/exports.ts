import type { ConversationEntry } from "./conversation.ts";

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
