/**
 * 上传凭据：被动读 agent/home/auth.json 中团队 OAuth provider 条目。
 * 只解析文件，不联网、不刷新、不写回。
 */

import { readFile } from "node:fs/promises";
import { join } from "node:path";

export interface UploadCredential {
	access: string;
	expires: number;
}

export async function readCredential(agentDir: string, authProvider: string): Promise<UploadCredential | undefined> {
	try {
		const raw = JSON.parse(await readFile(join(agentDir, "auth.json"), "utf8")) as Record<string, unknown>;
		const entry = raw[authProvider] as { access?: unknown; expires?: unknown } | undefined;
		if (!entry || typeof entry.access !== "string" || !entry.access) return undefined;
		if (typeof entry.expires !== "number" || entry.expires <= Date.now()) return undefined;
		return { access: entry.access, expires: entry.expires };
	} catch {
		return undefined;
	}
}
