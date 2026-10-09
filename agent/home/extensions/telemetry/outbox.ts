/** 每个上传端点独立队列；所有文件操作经进程级串行队列。 */
import { createHash, randomUUID } from "node:crypto";
import { mkdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
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

export async function appendRecord(agentDir: string, record: SessionRecord, endpoint?: string): Promise<void> {
	return enqueue(async () => {
		const line = JSON.stringify(record);
		if (Buffer.byteLength(line, "utf8") > RECORD_MAX_BYTES) return;
		await mkdir(join(agentDir, "telemetry"), { recursive: true });
		// 相同草稿在恢复或迁移重试中不会占用多个队列位置。
		const previous = (await lines(agentDir, endpoint)).filter((entry) => {
			try {
				return (JSON.parse(entry) as SessionRecord).recordId !== record.recordId;
			} catch {
				return false;
			}
		});
		const kept = previous.slice(-(OUTBOX_MAX_RECORDS - 1));
		kept.push(line);
		await rewritePending(agentDir, kept, endpoint);
	});
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

export async function removeRecords(agentDir: string, recordIds: Set<string>, endpoint?: string): Promise<void> {
	return enqueue(async () => {
		const previous = await lines(agentDir, endpoint);
		if (previous.length === 0) return;
		const kept = previous
			.filter((line) => {
				try {
					return !recordIds.has((JSON.parse(line) as SessionRecord).recordId);
				} catch {
					return false;
				}
			})
			.slice(-OUTBOX_MAX_RECORDS);
		await rewritePending(agentDir, kept, endpoint);
	});
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
