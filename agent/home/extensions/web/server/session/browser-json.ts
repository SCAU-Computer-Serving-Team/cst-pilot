import { redactRecoveryPasswords } from "../../../diagnostics/bitlocker-password.ts";

/** 只处理发往浏览器的副本，不改变工具参数或模型上下文。 */
export function browserJson(value: unknown): string {
	return JSON.stringify(value, (key, item) => {
		if (key === "assistantMessageEvent") return undefined; // UI 只消费 message 快照，不转发含参数片段的 SDK 增量副本。
		if (key === "recoveryPassword" && item !== undefined) return "[BitLocker 恢复密码已隐藏]";
		return typeof item === "string" ? redactRecoveryPasswords(item) : item;
	});
}
