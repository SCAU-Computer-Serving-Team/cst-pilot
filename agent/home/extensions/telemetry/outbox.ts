/**
 * 待发队列：agent/home/telemetry/pending.jsonl，一行一条记录。
 * 全部读写经 shared 的串行队列，多实例互斥；尽力而为，不承诺送达。
 */

import { randomUUID } from "node:crypto";
import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { join } from "node:path";
import type { SessionRecord } from "./record.ts";
import { enqueue } from "./shared.ts";

export const OUTBOX_MAX_RECORDS = 200;
export const BATCH_MAX_RECORDS = 50;
/** 单条记录超过此字节数直接丢弃：发送必被 413 拒收，留在队列只会堵住后续。 */
export const RECORD_MAX_BYTES = 1024 * 1024;

function pendingPath(agentDir: string): string {
	return join(agentDir, "telemetry", "pending.jsonl");
}

export async function appendRecord(agentDir: string, record: SessionRecord): Promise<void> {
	return enqueue(async () => {
		const line = `${JSON.stringify(record)}\n`;
		if (Buffer.byteLength(line, "utf8") > RECORD_MAX_BYTES) return;
		await mkdir(join(agentDir, "telemetry"), { recursive: true });
		await writeFile(pendingPath(agentDir), line, { flag: "a" });
	});
}

export async function readAll(agentDir: string): Promise<SessionRecord[]> {
	return enqueue(async () => {
		let text: string;
		try {
			text = await readFile(pendingPath(agentDir), "utf8");
		} catch {
			return [];
		}
		return text
			.split("\n")
			.filter((line) => line.trim() !== "")
			.map((line) => {
				try {
					return JSON.parse(line) as SessionRecord;
				} catch {
					return undefined;
				}
			})
			.filter((record): record is SessionRecord => record !== undefined);
	});
}

export async function removeRecords(agentDir: string, recordIds: Set<string>): Promise<void> {
	return enqueue(async () => {
		let text: string;
		try {
			text = await readFile(pendingPath(agentDir), "utf8");
		} catch {
			return;
		}
		const kept = text
			.split("\n")
			.filter((line) => line.trim() !== "")
			.filter((line) => {
				try {
					return !recordIds.has((JSON.parse(line) as SessionRecord).recordId);
				} catch {
					return false; // 损坏行借机清除。
				}
			});
		// 超限丢最旧。
		while (kept.length > OUTBOX_MAX_RECORDS) kept.shift();
		const temp = `${pendingPath(agentDir)}.${randomUUID()}.tmp`;
		await writeFile(temp, kept.length ? `${kept.join("\n")}\n` : "", "utf8");
		await rename(temp, pendingPath(agentDir));
	});
}
