/**
 * 实例草稿：agent/home/telemetry/drafts/<recordId>.json。
 * 原子替换；最终快照全部入队后删除。恢复兼容按 sessionId 命名的草稿。
 * 每文件事务锁协调保存和恢复，锁内不等待账单或端点队列。
 */

import { randomUUID } from "node:crypto";
import { readdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import type { BillingRequest } from "./billing.ts";
import { diagnose } from "./diagnostics.ts";
import { type DraftOwner, ownerAlive } from "./draft-owner.ts";
import { withFileLock } from "./file-lock.ts";
import { crashRecord, type SessionRecord } from "./record.ts";

export interface DraftFile {
	record: SessionRecord;
	lastTurnEndedAt: number;
	ownerPid?: number;
	owner?: DraftOwner;
	cstoaRequests?: BillingRequest[];
}

export function draftsDir(agentDir: string): string {
	return join(agentDir, "telemetry", "drafts");
}

function draftPath(agentDir: string, sessionId: string): string {
	return join(draftsDir(agentDir), `${encodeURIComponent(sessionId)}.json`);
}

export async function writeDraft(agentDir: string, sessionId: string, draft: DraftFile): Promise<void> {
	const target = draftPath(agentDir, sessionId);
	await withFileLock(target, () => replaceDraft(target, draft));
}

async function replaceDraft(target: string, draft: DraftFile): Promise<void> {
	const temp = `${target}.${randomUUID()}.tmp`;
	try {
		await writeFile(temp, JSON.stringify({ ...draft, ownerPid: draft.ownerPid ?? process.pid }), "utf8");
		await rename(temp, target);
	} catch (error) {
		await rm(temp, { force: true }).catch(() => undefined);
		throw error;
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
	prepare?: (record: SessionRecord, draft: DraftFile) => Promise<SessionRecord>,
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
			const target = join(dir, name);
			const candidate = await withFileLock(target, async () => {
				const raw = await readFile(target, "utf8");
				const draft = JSON.parse(raw) as DraftFile;
				if (await ownerAlive(draft.owner)) return;
				const record = draft.record;
				if (!record || !(record.turns > 0 || record.prompts > 0 || (record.cstoaBilling?.requests ?? 0) > 0)) {
					await rm(target, { force: true });
					return;
				}
				return { raw, draft };
			});
			if (!candidate) continue;
			const { draft, raw } = candidate;
			let recovered = draft.record.endReason ? draft.record : crashRecord(draft.record, draft.lastTurnEndedAt ?? 0);
			if (!draft.record.endReason && prepare) recovered = await prepare(recovered, draft);
			const sealed = await withFileLock(target, async () => {
				if ((await readFile(target, "utf8")) !== raw) return;
				// 入队前封存恢复结果；其他恢复进程只能重用此版本。
				await replaceDraft(target, { ...draft, owner: undefined, record: recovered });
				return readFile(target, "utf8");
			});
			if (sealed === undefined) continue;
			await diagnose(agentDir, {
				saved: { recordId: recovered.recordId, revision: recovered.revision, final: recovered.final },
			});
			await persist(recovered);
			records.push(recovered);
			await withFileLock(target, async () => {
				if ((await readFile(target, "utf8")) === sealed) await rm(target, { force: true });
			});
		} catch {
			// 损坏或入队失败时保留源文件，后续启动重试。
		}
	}
	return records;
}
