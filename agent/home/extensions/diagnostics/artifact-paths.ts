import { mkdir, realpath } from "node:fs/promises";
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { ROOT_DIR } from "./runtime.ts";

export const ARTIFACTS_DIR = join(ROOT_DIR, "artifacts");
export const PLAN_DIR = join(ROOT_DIR, "agent", "home", "command-plans");

/** 日期用于目录名；按本机日期默认，拒绝溢出日期和路径字符。 */
export function artifactDate(value?: string, now = new Date()): string {
	const day =
		value ??
		`${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}-${String(now.getDate()).padStart(2, "0")}`;
	const parsed = new Date(`${day}T00:00:00Z`);
	if (
		!/^[1-9]\d{3}-\d{2}-\d{2}$/.test(day) ||
		!Number.isFinite(parsed.getTime()) ||
		parsed.toISOString().slice(0, 10) !== day
	)
		throw new Error("请求的产物日期无效，请使用 YYYY-MM-DD");
	return day;
}

export function artifactName(title: string): string {
	const cleaned = title
		.replace(/[\\/:*?"<>|\u0000-\u001f]/g, "")
		.replace(/\s+/g, " ")
		.trim();
	return (
		Array.from(cleaned)
			.slice(0, 60)
			.join("")
			.replace(/[. ]+$/, "") || "会话"
	);
}

export function artifactDirectory(root: string, day: string, kind: "runbook" | "sessions"): string {
	return join(root, artifactDate(day), kind);
}

/** 每层检查真实路径，防止符号链接或 Windows junction 改变读写边界。 */
async function checkedDirectory(
	root: string,
	day: string,
	kind: "runbook" | "sessions",
	create: boolean,
): Promise<string> {
	if (create) await mkdir(root, { recursive: true });
	const base = await realpath(root);
	const parent = await realpath(dirname(resolve(root)));
	const actual = relative(parent, base),
		expected = basename(resolve(root));
	if (process.platform === "win32" ? actual.toLowerCase() !== expected.toLowerCase() : actual !== expected)
		throw new Error("产物根目录指向其他位置");
	let dir = root;
	for (const part of [artifactDate(day), kind]) {
		dir = join(dir, part);
		if (create) await mkdir(dir, { recursive: true });
		const path = relative(base, await realpath(dir));
		if (isAbsolute(path) || path === ".." || path.startsWith(`..${sep}`)) throw new Error("产物目录指向工具包外");
	}
	return dir;
}

export function ensureArtifactDirectory(root: string, day: string, kind: "runbook" | "sessions"): Promise<string> {
	return checkedDirectory(root, day, kind, true);
}

export function existingArtifactDirectory(root: string, day: string, kind: "runbook" | "sessions"): Promise<string> {
	return checkedDirectory(root, day, kind, false);
}
