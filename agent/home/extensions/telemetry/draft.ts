/**
 * 轮次级草稿：进行中会话的落盘形态，agent/home/telemetry/drafts/<sessionId>.json。
 * 每 turn_end 整体重写（先写临时文件再改名）；定稿后删除；进程启动时恢复残留草稿。
 * 按会话分文件，多实例并行写互不干扰，不经共享串行队列。
 */

import { randomUUID } from "node:crypto";
import { mkdir, readdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { crashRecord, type SessionRecord } from "./record.ts";

export interface DraftFile {
	record: SessionRecord;
	lastTurnEndedAt: number;
	ownerPid?: number;
}

export function draftsDir(agentDir: string): string {
	return join(agentDir, "telemetry", "drafts");
}

function draftPath(agentDir: string, sessionId: string): string {
	return join(draftsDir(agentDir), `${encodeURIComponent(sessionId)}.json`);
}

export async function writeDraft(agentDir: string, sessionId: string, draft: DraftFile): Promise<void> {
	const target = draftPath(agentDir, sessionId);
	const temp = `${target}.${randomUUID()}.tmp`;
	try {
		await mkdir(draftsDir(agentDir), { recursive: true });
		await writeFile(temp, JSON.stringify({ ...draft, ownerPid: draft.ownerPid ?? process.pid }), "utf8");
		await rename(temp, target);
	} catch {
		await rm(temp, { force: true }).catch(() => undefined);
	}
}

export async function removeDraft(agentDir: string, sessionId: string): Promise<void> {
	await rm(draftPath(agentDir, sessionId), { force: true }).catch(() => undefined);
}

/**
 * 启动恢复：未定稿记录补为 crash，已定稿记录保持原值。
 * persist 确认入队后才删除源草稿，返回成功恢复的记录；全空草稿直接删除。
 */
export async function recoverDrafts(
	agentDir: string,
	persist: (record: SessionRecord) => Promise<void>,
): Promise<SessionRecord[]> {
	const dir = draftsDir(agentDir);
	let files: string[];
	try {
		files = (await readdir(dir)).filter((name) => name.endsWith(".json"));
	} catch {
		return [];
	}
	const records: SessionRecord[] = [];
	for (const name of files) {
		try {
			const draft = JSON.parse(await readFile(join(dir, name), "utf8")) as DraftFile;
			if (draft.ownerPid && draft.ownerPid !== process.pid) {
				try {
					process.kill(draft.ownerPid, 0);
					continue;
				} catch (error) {
					if ((error as NodeJS.ErrnoException).code !== "ESRCH") continue;
				}
			}
			const record = draft.record;
			if (record && (record.turns > 0 || record.prompts > 0)) {
				const recovered = record.endReason ? record : crashRecord(record, draft.lastTurnEndedAt ?? 0);
				await persist(recovered);
				records.push(recovered);
			}
			await rm(join(dir, name), { force: true });
		} catch {
			// 损坏或入队失败时保留源文件，后续启动重试。
		}
	}
	return records;
}
