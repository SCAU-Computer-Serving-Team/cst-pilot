/** 每个上传端点独立队列；所有文件操作经进程级串行队列。 */
import { createHash, randomUUID } from "node:crypto";
import { mkdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { withFileLock } from "./file-lock.ts";
import type { SessionRecord } from "./record.ts";
import { enqueue } from "./shared.ts";

export const OUTBOX_MAX_RECORDS = 200;
export const BATCH_MAX_RECORDS = 50;
export const RECORD_MAX_BYTES = 1024 * 1024;

function pendingPath(agentDir: string, endpoint?: string): string {
	const name = endpoint ? `pending-${createHash("sha256").update(endpoint).digest("hex")}.jsonl` : "pending.jsonl";
	return join(agentDir, "telemetry", name);
}

async function lines(agentDir: string, endpoint?: string): Promise<string[]> {
	const text = await readFile(pendingPath(agentDir, endpoint), "utf8").catch((error: NodeJS.ErrnoException) => {
		if (error.code === "ENOENT") return "";
		throw error;
	});
	return text.split("\n").filter((entry) => entry.trim() !== "");
}

export async function appendRecord(
	agentDir: string,
	record: SessionRecord,
	endpoint?: string,
): Promise<SessionRecord | undefined> {
	return enqueue(() =>
		withFileLock(pendingPath(agentDir, endpoint), async () => {
			const line = JSON.stringify(record);
			if (Buffer.byteLength(line, "utf8") > RECORD_MAX_BYTES) return;
			await mkdir(join(agentDir, "telemetry"), { recursive: true });
			const existing = await lines(agentDir, endpoint);
			if (record.v === "0.2") {
				for (const entry of existing) {
					try {
						const current = JSON.parse(entry) as SessionRecord;
						if (current.recordId === record.recordId && (current.revision ?? 0) >= (record.revision ?? 0))
							return current;
					} catch {
						/* 损坏行由下一次重写清理。 */
					}
				}
			}
			// 相同草稿在恢复或迁移重试中不会占用多个队列位置。
			const previous = existing.filter((entry) => {
				try {
					return (JSON.parse(entry) as SessionRecord).recordId !== record.recordId;
				} catch {
					return false;
				}
			});
			const kept = previous.slice(-(OUTBOX_MAX_RECORDS - 1));
			kept.push(line);
			await rewritePending(agentDir, kept, endpoint);
			return record;
		}),
	);
}

export async function readAll(agentDir: string, endpoint?: string): Promise<SessionRecord[]> {
	return enqueue(async () => {
		return (await lines(agentDir, endpoint)).flatMap((line) => {
			try {
				return [JSON.parse(line) as SessionRecord];
			} catch {
				return [];
			}
		});
	});
}

/** 仅删除实际发送的内容，发送期间产生的同实例更新继续保留。 */
export async function removeRecords(agentDir: string, records: SessionRecord[], endpoint?: string): Promise<void> {
	const acknowledged = new Set(records.map((record) => JSON.stringify(record)));
	return enqueue(() =>
		withFileLock(pendingPath(agentDir, endpoint), async () => {
			const previous = await lines(agentDir, endpoint);
			if (previous.length === 0) return;
			const kept = previous
				.filter((line) => {
					try {
						return !acknowledged.has(JSON.stringify(JSON.parse(line)));
					} catch {
						return false;
					}
				})
				.slice(-OUTBOX_MAX_RECORDS);
			await rewritePending(agentDir, kept, endpoint);
		}),
	);
}

async function rewritePending(agentDir: string, lines: string[], endpoint?: string): Promise<void> {
	const temp = `${pendingPath(agentDir, endpoint)}.${randomUUID()}.tmp`;
	try {
		await writeFile(temp, lines.length ? `${lines.join("\n")}\n` : "", "utf8");
		await rename(temp, pendingPath(agentDir, endpoint));
	} finally {
		await rm(temp, { force: true }).catch(() => undefined);
	}
}
