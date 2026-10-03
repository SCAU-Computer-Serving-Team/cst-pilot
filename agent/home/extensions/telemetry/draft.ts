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
		await writeFile(temp, JSON.stringify(draft), "utf8");
		await rename(temp, target);
	} catch {
		await rm(temp, { force: true }).catch(() => undefined);
	}
}

export async function removeDraft(agentDir: string, sessionId: string): Promise<void> {
	await rm(draftPath(agentDir, sessionId), { force: true }).catch(() => undefined);
}

/**
 * 进程启动恢复：每个残留草稿补成 endReason = crash 的记录（endedAt 取最后轮次结束时刻）。
 * 返回值供入口层写入待发队列。全空会话（无轮次数据）不产生记录，直接删草稿。
 */
export async function recoverDrafts(agentDir: string): Promise<SessionRecord[]> {
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
			const record = draft.record;
			if (record && (record.turns > 0 || record.prompts > 0)) {
				records.push(crashRecord(record, draft.lastTurnEndedAt ?? 0));
			}
		} catch {
			// 草稿损坏则丢弃，不阻塞其余恢复。
		} finally {
			await rm(join(dir, name), { force: true }).catch(() => undefined);
		}
	}
	return records;
}
