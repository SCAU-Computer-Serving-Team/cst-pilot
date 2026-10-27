import { createHash, randomUUID } from "node:crypto";
import { mkdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { withFileLock } from "./file-lock.ts";

export type FailureKind = "storage" | "auth" | "capability" | "protocol" | "network" | "timeout" | "tls" | "http";
interface SnapshotRef {
	recordId: string;
	revision?: number;
	final?: boolean;
}
interface AckRef extends SnapshotRef {
	status: "confirmed" | "rejected";
}
interface EndpointStatus {
	queued?: (SnapshotRef & { at: string })[];
	acks?: AckRef[];
	acknowledgedAt?: string;
	heartbeat?: { runId: string; sequence: number; acknowledgedAt: string };
	failure?: { kind: FailureKind; at: string };
}
interface DiagnosticState {
	v: 1;
	updatedAt: string;
	saved: (SnapshotRef & { at: string })[];
	endpoints: Record<string, EndpointStatus>;
	failure?: { kind: FailureKind; at: string };
}
type DiagnosticEvent =
	| { saved: SnapshotRef }
	| { failure: FailureKind; endpoint?: string }
	| { endpoint: string; acks: AckRef[] }
	| { endpoint: string; queued: SnapshotRef }
	| { endpoint: string; heartbeat: { runId: string; sequence: number } };

/** 有界本地状态，不记录正文、凭据、原始端点或设备标识；诊断写入失败不改变投递结果。 */
export async function diagnose(agentDir: string, event: DiagnosticEvent): Promise<void> {
	const dir = join(agentDir, "telemetry");
	const path = join(dir, "status.json");
	const temporary = `${path}.${randomUUID()}.tmp`;
	try {
		await mkdir(dir, { recursive: true });
		await withFileLock(path, async () => {
			const now = new Date().toISOString();
			let state: DiagnosticState = { v: 1, updatedAt: now, saved: [], endpoints: {} };
			try {
				const raw = await readFile(path, "utf8");
				if (Buffer.byteLength(raw) <= 64 * 1024) {
					const parsed = JSON.parse(raw) as DiagnosticState;
					if (parsed.v === 1 && Array.isArray(parsed.saved) && parsed.endpoints) state = parsed;
				}
			} catch {
				/* 初次写入或损坏状态从空记录开始。 */
			}
			state.updatedAt = now;
			if ("saved" in event) {
				state.saved = [
					...state.saved.filter((entry) => entry.recordId !== event.saved.recordId),
					{ ...event.saved, at: now },
				].slice(-50);
			} else {
				const key = event.endpoint
					? createHash("sha256").update(event.endpoint).digest("hex").slice(0, 16)
					: undefined;
				if (key) state.endpoints[key] ??= {};
				const endpoint = key ? state.endpoints[key] : undefined;
				if ("failure" in event) {
					const failure = { kind: event.failure, at: now };
					if (endpoint) endpoint.failure = failure;
					else state.failure = failure;
				} else if (endpoint && "queued" in event) {
					endpoint.queued = [
						...(endpoint.queued ?? []).filter((entry) => entry.recordId !== event.queued.recordId),
						{ ...event.queued, at: now },
					].slice(-20);
				} else if (endpoint && "acks" in event && event.acks.length > 0) {
					const ids = new Set(event.acks.map((entry) => entry.recordId));
					endpoint.acks = [
						...(endpoint.acks ?? []).filter((entry) => !ids.has(entry.recordId)),
						...event.acks,
					].slice(-20);
					endpoint.acknowledgedAt = now;
				} else if (endpoint && "heartbeat" in event)
					endpoint.heartbeat = { ...event.heartbeat, acknowledgedAt: now };
				state.endpoints = Object.fromEntries(Object.entries(state.endpoints).slice(-8));
			}
			await writeFile(temporary, JSON.stringify(state), "utf8");
			await rename(temporary, path);
		});
	} catch {
		/* 诊断失败不删除队列或阻塞业务。 */
	} finally {
		await rm(temporary, { force: true }).catch(() => undefined);
	}
}
