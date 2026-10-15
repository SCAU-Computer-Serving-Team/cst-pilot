import type { UserMessage } from "@earendil-works/pi-ai";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import type { BitLockerParams } from "./bitlocker-core.ts";
import { normalizeDrive, redactRecoveryPasswords } from "./bitlocker-data.ts";

export const HIDDEN_RECOVERY_PASSWORD = "[BitLocker 恢复密码已隐藏]";

/** Keep credentials transient while replacing finalized messages before Pi persists them. */
export function bitLockerCredentials(pi: ExtensionAPI) {
	const calls = new Map<string, { drive?: string; scope?: string; password: string }>();
	const users = new Map<number, UserMessage>();
	const clear = () => {
		calls.clear();
		users.clear();
	};

	function scrub(value: object): void {
		const object = value as Record<string, unknown>;
		for (const [key, item] of Object.entries(object)) {
			if (key === "recoveryPassword" && typeof item === "string") object[key] = HIDDEN_RECOVERY_PASSWORD;
			else if (typeof item === "string") object[key] = redactRecoveryPasswords(item);
			else if (item && typeof item === "object") scrub(item);
		}
	}
	pi.registerMarkdownTransformer((text) => redactRecoveryPasswords(text));
	pi.on("message_end", (event) => {
		const message = event.message;
		if (message.role === "user") {
			const text =
				typeof message.content === "string"
					? message.content
					: message.content
							.filter((p) => p.type === "text")
							.map((p) => p.text)
							.join("\n");
			if (redactRecoveryPasswords(text) !== text) users.set(message.timestamp, structuredClone(message));
		}
		if (message.role === "assistant") {
			for (const part of message.content) {
				if (part.type !== "toolCall" || part.name !== "bitlocker") continue;
				const password = part.arguments.recoveryPassword;
				if (typeof password === "string" && password !== HIDDEN_RECOVERY_PASSWORD) {
					calls.set(part.id, {
						drive: typeof part.arguments.drive === "string" ? part.arguments.drive : undefined,
						scope: typeof part.arguments.scope === "string" ? part.arguments.scope : undefined,
						password,
					});
				}
			}
		}
		const replacement = structuredClone(message);
		scrub(replacement);
		return { message: replacement };
	});
	// A request-local copy preserves the AI-filled parameter path without storing the raw user message.
	pi.on("context", (event) => ({
		messages: event.messages.map((message) =>
			message.role === "user" && users.has(message.timestamp)
				? structuredClone(users.get(message.timestamp) ?? message)
				: message,
		),
	}));
	pi.on("agent_end", clear);
	pi.on("session_start", clear);
	pi.on("session_shutdown", clear);
	pi.on("tool_result", (event) => {
		if (event.toolName === "bitlocker") calls.delete(event.toolCallId);
	});
	return {
		claim(id: string, params: BitLockerParams): string | undefined {
			const saved = calls.get(id);
			calls.delete(id);
			if (saved) {
				if (
					saved.scope !== "unlock" ||
					params.scope !== "unlock" ||
					normalizeDrive(saved.drive) !== normalizeDrive(params.drive)
				) {
					throw new Error("BitLocker 凭据与目标调用不匹配");
				}
				return saved.password;
			}
			if (params.recoveryPassword === HIDDEN_RECOVERY_PASSWORD) throw new Error("BitLocker 凭据已过期，请重新输入");
			return params.recoveryPassword;
		},
	};
}
