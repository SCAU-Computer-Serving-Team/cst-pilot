import { spawn } from "node:child_process";
import { resolve } from "node:path";

/** 路径由已完成的导出登记校验后传入；不接收网页提交的任意路径。 */
export function openExportDirectory(directory: string): Promise<void> {
	if (process.platform !== "win32") return Promise.reject(new Error("当前系统不支持本地目录打开"));
	return new Promise((resolveLaunch, reject) => {
		const child = spawn(resolve(process.env.SystemRoot ?? "C:/Windows", "explorer.exe"), [directory], {
			detached: true,
			stdio: "ignore",
			windowsHide: false,
		});
		child.once("error", reject);
		child.once("spawn", () => {
			child.unref();
			resolveLaunch();
		});
	});
}
