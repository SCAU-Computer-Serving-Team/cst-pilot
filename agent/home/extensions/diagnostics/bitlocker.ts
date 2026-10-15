import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Text } from "@earendil-works/pi-tui";
import { Type } from "typebox";
import { executeBitLocker } from "./bitlocker-core.ts";
import { bitLockerCredentials, HIDDEN_RECOVERY_PASSWORD } from "./bitlocker-credentials.ts";
import { redactRecoveryPasswords } from "./bitlocker-data.ts";
import { bitLockerInteraction } from "./bitlocker-tui.ts";
import { bitLockerWebInteractions } from "./bitlocker-web.ts";
import { createBitLockerRunner } from "./bitlocker-windows.ts";
import { diagnosticResult, OUTPUT_GUIDELINE } from "./result.ts";

export default function registerBitLocker(pi: ExtensionAPI): void {
	const credentials = bitLockerCredentials(pi);
	const run = createBitLockerRunner();
	pi.registerTool({
		name: "bitlocker",
		label: "BitLocker",
		description:
			"查询 BitLocker 卷状态、使用 48 位恢复密码解锁、关闭加密并发起完整解密。status 默认查询全部卷；unlock 必须指定盘符，省略恢复密码时使用本地隐藏输入。decrypt 默认全部可枚举卷，执行前在本地界面确认固定目标。支持 TUI 和 Web；解锁保留加密，解密在 Windows 后台进行。",
		promptSnippet: "Query BitLocker, unlock a selected drive, or request full decryption after local confirmation",
		promptGuidelines: [
			OUTPUT_GUIDELINE,
			"Read the bitlocker skill at its discovered location before unlock or decrypt. Only execute these scopes when explicitly requested by the user.",
			"Prefer omitting recoveryPassword so the user enters it locally in the TUI or Web interface. If the user explicitly chooses AI parameters, explain that the credential enters model context and may reach the model provider.",
			"When the key is missing, guide the user to https://aka.ms/myrecoverykey and match the recovery key ID. Never request the Microsoft account password or a Windows PIN.",
			"Decrypt is asynchronous. Report each volume's operation and actual state separately. Only conversionStatus=0 confirms full decryption; never blindly retry an unknown operation.",
		],
		executionMode: "sequential",
		parameters: Type.Object({
			scope: Type.Optional(Type.Union([Type.Literal("status"), Type.Literal("unlock"), Type.Literal("decrypt")])),
			drive: Type.Optional(Type.String({ description: "盘符，如 C 或 C:；unlock 必填，其他 scope 省略时为全部卷" })),
			recoveryPassword: Type.Optional(
				Type.String({ description: "unlock 专用，八组六位数字的恢复密码；默认省略，使用本地独立输入" }),
			),
		}),
		renderCall(args, theme) {
			const drive =
				typeof args.drive === "string" && /^[a-z]:?$/i.test(args.drive)
					? args.drive.toUpperCase()
					: "全部卷/待确定";
			return new Text(
				`${theme.fg("toolTitle", "bitlocker")} ${theme.fg("toolOutput", `${args.scope ?? "status"} ${drive}${args.recoveryPassword === undefined ? "" : " · 恢复密码已隐藏"}`)}`,
				0,
				0,
			);
		},
		renderResult(result, _options, theme) {
			const text = result.content
				.filter((part) => part.type === "text")
				.map((part) => part.text)
				.join("\n");
			return new Text(theme.fg("toolOutput", redactRecoveryPasswords(text)), 0, 0);
		},
		async execute(toolCallId, params, signal, onUpdate, ctx) {
			let password = credentials.claim(toolCallId, params);
			if (params.recoveryPassword !== undefined) params.recoveryPassword = HIDDEN_RECOVERY_PASSWORD;
			try {
				const result = await executeBitLocker(
					{ ...params, recoveryPassword: password },
					run,
					bitLockerWebInteractions
						.get(ctx.sessionManager?.getSessionId())
						?.forCall(
							toolCallId,
							[...(ctx.sessionManager?.getBranch?.() ?? [])]
								.reverse()
								.find(
									(entry) =>
										entry.type === "message" &&
										entry.message.role === "assistant" &&
										entry.message.content.some((part) => part.type === "toolCall" && part.id === toolCallId),
								)?.id,
						) ?? bitLockerInteraction(ctx),
					signal,
					(stage) => onUpdate?.(diagnosticResult({ bitlocker: { stage } })),
				);
				// Also sanitize unexpected numeric credentials in provider error text.
				const safe = JSON.parse(redactRecoveryPasswords(JSON.stringify(result)));
				return diagnosticResult(safe);
			} catch (error) {
				throw new Error(redactRecoveryPasswords(error instanceof Error ? error.message : "BitLocker 执行失败"));
			} finally {
				password = undefined;
			}
		},
	});
}
