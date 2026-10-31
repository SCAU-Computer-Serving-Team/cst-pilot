import { artifactDate } from "../../../diagnostics/artifact-paths.ts";
import type { ConversationEntry, Message } from "../../shared/conversation.ts";
import type { ExportOptions, ExportSnapshot } from "../../shared/exports.ts";
import { browserJson } from "../session/browser-json.ts";
import type { WebSessionSlot } from "../session/sessions.ts";

export class ExportError extends Error {
	readonly status: number;
	readonly code: string;
	constructor(status: number, code: string, message: string) {
		super(message);
		this.status = status;
		this.code = code;
	}
}

export function exportOptions(input: Record<string, unknown>): ExportOptions {
	if (Object.keys(input).some((key) => !["format", "theme", "includeThinking", "includeTools"].includes(key)))
		throw new ExportError(400, "invalid_export", "请求包含未知的导出选项");
	const { format = "png", theme = "light", includeThinking = false, includeTools = false } = input;
	if (
		(format !== "png" && format !== "html") ||
		(theme !== "light" && theme !== "dark") ||
		typeof includeThinking !== "boolean" ||
		typeof includeTools !== "boolean"
	)
		throw new ExportError(400, "invalid_export", "请求的导出选项无效");
	return {
		format,
		theme,
		includeThinking,
		includeTools,
	};
}

/** 同一入口取当前分支；仅投影显示字段，省略 SDK 元数据与内部配置。 */
export function conversationEntries(slot: WebSessionSlot): ConversationEntry[] {
	return slot.session.sessionManager.getBranch().flatMap((entry): ConversationEntry[] => {
		if (entry.type === "branch_summary")
			return [
				{
					id: entry.id,
					parentId: entry.parentId,
					message: {
						role: "branchSummary",
						content: "",
						summary: entry.summary,
						fromId: entry.fromId,
						timestamp: new Date(entry.timestamp).getTime(),
					},
				},
			];
		if (entry.type !== "message") return [];
		const source = slot.execution.decorate(entry.message);
		if (source.role !== "user" && source.role !== "assistant" && source.role !== "toolResult") return [];
		const message: Message = {
			role: source.role,
			content: source.content,
			timestamp: source.timestamp,
			...(source.role === "assistant" ? { stopReason: source.stopReason, errorMessage: source.errorMessage } : {}),
			...(source.role === "toolResult"
				? {
						toolName: source.toolName,
						toolCallId: source.toolCallId,
						details: source.details,
						isError: source.isError,
					}
				: {}),
		};
		return [{ id: entry.id, parentId: entry.parentId, message }];
	});
}

export function exportSnapshot(
	sessionId: string,
	title: string,
	entries: ConversationEntry[],
	options: ExportOptions,
): ExportSnapshot {
	const capturedAt = new Date();
	const chosen = entries.flatMap((entry): ConversationEntry[] => {
		const message = entry.message;
		if (message.role === "toolResult" && !options.includeTools) return [];
		const content =
			typeof message.content === "string"
				? message.content
				: message.content
						.filter((part) =>
							part.type === "thinking"
								? options.includeThinking && !part.redacted
								: part.type === "toolCall"
									? options.includeTools
									: true,
						)
						.map((part) =>
							part.type === "toolCall"
								? { type: part.type, id: part.id, name: part.name, arguments: part.arguments }
								: part,
						);
		if (
			message.role === "assistant" &&
			Array.isArray(content) &&
			!content.length &&
			!message.errorMessage &&
			message.stopReason !== "aborted"
		)
			return [];
		// 使用新的消息对象，清除执行态与嵌套的结果副本；脱敏只作用于导出副本。
		return [{ id: entry.id, message: { ...message, content } }];
	});
	if (!chosen.length) throw new ExportError(400, "empty_export", "当前没有可导出的消息");
	const serialized = browserJson({
		sessionId,
		title,
		capturedAt: capturedAt.toISOString(),
		options: { ...options, date: artifactDate(undefined, capturedAt) },
		entries: chosen,
	});
	if (Buffer.byteLength(serialized) > 32 * 1024 * 1024)
		throw new ExportError(413, "export_too_large", "会话内容过大，请缩短当前分支后导出");
	return JSON.parse(serialized) as ExportSnapshot;
}
