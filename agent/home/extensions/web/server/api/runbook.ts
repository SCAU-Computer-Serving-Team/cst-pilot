import { execFile } from "node:child_process";
import { realpath, stat } from "node:fs/promises";
import { dirname, extname, isAbsolute, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../../../../../..");
export class RunbookOpenError extends Error {
	readonly status: number;
	readonly code: string;
	constructor(status: number, code: string, message: string) {
		super(message);
		this.status = status;
		this.code = code;
	}
}
export async function openDeliveredRunbook(
	file: string,
	open = openTextFile,
	root = resolve(ROOT, "outbox"),
): Promise<void> {
	let target: string;
	try {
		const paths = await Promise.all([realpath(root), realpath(file)]);
		const base = paths[0];
		target = paths[1];
		const path = relative(base, target);
		if (isAbsolute(path) || path === ".." || path.startsWith(`..${sep}`) || extname(target).toLowerCase() !== ".txt")
			throw new RunbookOpenError(400, "runbook_invalid_file", "文件不属于已交付的文本清单");
		if (!(await stat(target)).isFile())
			throw new RunbookOpenError(400, "runbook_invalid_file", "交付路径未指向文本文件");
	} catch (error) {
		if (error instanceof RunbookOpenError) throw error;
		const code = (error as NodeJS.ErrnoException).code;
		if (code === "ENOENT" || code === "ENOTDIR")
			throw new RunbookOpenError(404, "runbook_missing", "清单文件已不存在，请使用卡片中的命令或重新生成清单。");
		if (code === "EACCES" || code === "EPERM")
			throw new RunbookOpenError(403, "runbook_access_denied", "没有权限访问清单，请检查文件权限。");
		throw new RunbookOpenError(500, "runbook_unavailable", "无法读取清单，请检查所在介质。");
	}
	try {
		await open(target);
	} catch {
		throw new RunbookOpenError(503, "runbook_launch_failed", "无法启动记事本，可使用详情中的路径手动打开。");
	}
}
function openTextFile(file: string): Promise<void> {
	if (process.platform !== "win32") throw new Error("当前系统不支持本地清单打开");
	return new Promise((resolveLaunch, reject) => {
		const child = execFile(resolve(process.env.SystemRoot ?? "C:/Windows", "System32/notepad.exe"), [file], {
			windowsHide: false,
		});
		child.once("spawn", () => resolveLaunch());
		child.once("error", () => reject(new Error("无法启动记事本")));
	});
}
