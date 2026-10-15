import { spawn } from "node:child_process";
import { BITLOCKER_COMMAND } from "./bitlocker-commands.ts";
import { type BitLockerRunner, parseBitLockerReply } from "./bitlocker-data.ts";
import { decodeBuffer, PWSH } from "./runtime.ts";

/** Never interpolate request data into scripts, argv, exception text or temporary files. */
export function createBitLockerRunner(): BitLockerRunner {
	return async (request, signal) => {
		signal?.throwIfAborted();
		if (process.platform !== "win32") throw new Error("BitLocker 工具仅支持 Windows");
		const raw = await new Promise<string>((resolve, reject) => {
			const child = spawn(
				PWSH,
				["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", BITLOCKER_COMMAND],
				{ windowsHide: true, stdio: ["pipe", "pipe", "pipe"] },
			);
			const chunks: Buffer[] = [];
			let bytes = 0;
			let settled = false;
			const finish = (error?: Error) => {
				if (settled) return;
				settled = true;
				clearTimeout(timer);
				signal?.removeEventListener("abort", abort);
				if (error) {
					child.kill();
					reject(error);
				} else resolve(decodeBuffer(Buffer.concat(chunks)));
			};
			const abort = () => finish(new Error("BitLocker 调用已中断；已提交的操作需重新查询状态"));
			const timer = setTimeout(() => finish(new Error("BitLocker 调用超时；需重新查询状态")), 60_000);
			signal?.addEventListener("abort", abort, { once: true });
			child.on("error", () => finish(new Error("BitLocker PowerShell 子进程无法启动")));
			child.stdin.on("error", () => finish(new Error("BitLocker 请求传输失败；需重新查询状态")));
			child.stdout.on("data", (chunk: Buffer) => {
				bytes += chunk.length;
				if (bytes > 16 * 1024 * 1024) finish(new Error("BitLocker 接口输出超出限制"));
				else chunks.push(chunk);
			});
			// Windows exceptions can include argument values. Do not retain stderr.
			child.stderr.resume();
			child.on("close", (code) =>
				finish(code === 0 ? undefined : new Error("BitLocker 子进程执行失败；需重新查询状态")),
			);
			if (signal?.aborted) abort();
			else child.stdin.end(JSON.stringify(request));
		});
		try {
			return parseBitLockerReply(JSON.parse(raw));
		} catch {
			throw new Error("BitLocker 接口返回无效数据；已提交的操作需重新查询状态");
		}
	};
}
