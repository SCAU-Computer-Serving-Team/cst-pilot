import { execFile } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { diagnosticCommand } from "./pwsh-data.ts";

export const ROOT_DIR = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..", "..");
export const PWSH = join(ROOT_DIR, "pwsh", "pwsh.exe");
export const execFileP = promisify(execFile);

export function errorMessage(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

/** Windows error streams may still use the system ANSI code page. */
export function decodeBuffer(buffer: Buffer | undefined): string {
	if (!buffer?.length) return "";
	try {
		return new TextDecoder("utf-8", { fatal: true }).decode(buffer);
	} catch {
		try {
			return new TextDecoder("gbk").decode(buffer);
		} catch {
			return buffer.toString("latin1");
		}
	}
}

function stripAnsi(text: string): string {
	return text.replace(/\u001b\[[0-9;]*[A-Za-z]/g, "").replace(/[\u0000-\u0008\u000b\u000c\u000e-\u001f]/g, "");
}

/** Configure collection policy once; cancellation belongs to each invocation. */
export function createPwshRunner(options: { timeoutMs: number; diagnostics?: boolean; path?: string }) {
	return async function runPwsh(
		command: string,
		invocation: { timeoutMs?: number; path?: string; signal?: AbortSignal } = {},
	): Promise<unknown> {
		const { signal } = invocation;
		const timeoutMs = invocation.timeoutMs ?? options.timeoutMs;
		const pwshPath = invocation.path ?? options.path ?? PWSH;
		signal?.throwIfAborted();
		try {
			const result = await execFileP(
				pwshPath,
				[
					"-NoProfile",
					"-NonInteractive",
					"-ExecutionPolicy",
					"Bypass",
					"-Command",
					options.diagnostics
						? diagnosticCommand(command)
						: `[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false);\n${command}`,
				],
				{ signal, timeout: timeoutMs, windowsHide: true, encoding: "buffer", maxBuffer: 16 * 1024 * 1024 },
			);
			const stdout = decodeBuffer(result.stdout);
			if (!stdout.trim())
				return { error: (stripAnsi(decodeBuffer(result.stderr)).trim() || "空输出").slice(0, 500) };
			try {
				return JSON.parse(stdout);
			} catch {
				return JSON.parse(stripAnsi(stdout));
			}
		} catch (error) {
			signal?.throwIfAborted();
			const stderr =
				error instanceof Error && "stderr" in error && Buffer.isBuffer(error.stderr)
					? stripAnsi(decodeBuffer(error.stderr)).trim()
					: "";
			return { error: (stderr || errorMessage(error)).slice(0, 500) };
		}
	};
}

export function asRecord(value: unknown): Record<string, unknown> {
	if (value === null || typeof value !== "object" || Array.isArray(value)) throw new Error("采集输出应为 JSON 对象");
	return value as Record<string, unknown>;
}

export function asRecords(value: unknown): Record<string, unknown>[] {
	if (!Array.isArray(value)) throw new Error("采集输出应为 JSON 数组");
	return value.map(asRecord);
}
