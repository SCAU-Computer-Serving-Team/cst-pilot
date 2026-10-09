import type { TelemetryEndpoint } from "./config.ts";
import { appendRecord, BATCH_MAX_RECORDS, readAll, removeRecords } from "./outbox.ts";
import type { SessionRecord } from "./record.ts";
import { shared } from "./shared.ts";
import { sendBatch } from "./transport.ts";

const BATCH_MAX_BYTES = 900 * 1024;

/** 同一 recordId 写入全部目标；一端确认不会删除另一端的队列。 */
export async function queueRecord(
	agentDir: string,
	record: SessionRecord,
	endpoints: TelemetryEndpoint[],
): Promise<void> {
	const results = await Promise.allSettled(endpoints.map((endpoint) => appendRecord(agentDir, record, endpoint.url)));
	if (results.some((result) => result.status === "rejected")) throw new Error("遥测队列写入失败");
}

/** 先复制到所有目标，再清除单端点队列；中断重试使用 recordId 去重。 */
export async function migratePending(agentDir: string, endpoints: TelemetryEndpoint[]): Promise<void> {
	if (endpoints.length === 0) return;
	const pending = await readAll(agentDir);
	for (const record of pending) await queueRecord(agentDir, record, endpoints);
	await removeRecords(agentDir, new Set(pending.map((record) => record.recordId)));
}

export async function flushEndpoints(agentDir: string, access: string, endpoints: TelemetryEndpoint[]): Promise<void> {
	await Promise.allSettled(endpoints.map((endpoint) => flushEndpoint(agentDir, access, endpoint)));
}

async function flushEndpoint(agentDir: string, access: string, endpoint: TelemetryEndpoint): Promise<void> {
	if (endpoint.unavailable) return;
	const deliveries = shared().deliveries;
	const key = `${agentDir}\0${endpoint.url}`;
	let state = deliveries.get(key);
	if (!state) {
		state = { inflight: false, stopped: false, rerun: false };
		deliveries.set(key, state);
	}
	if (state.stopped) return;
	if (state.inflight) {
		state.rerun = true;
		return;
	}
	state.inflight = true;
	try {
		do {
			state.rerun = false;
			for (;;) {
				const pending = await readAll(agentDir, endpoint.url);
				if (pending.length === 0) break;
				const batch: SessionRecord[] = [];
				let bytes = 0;
				for (const record of pending) {
					const size = Buffer.byteLength(JSON.stringify(record), "utf8");
					if (batch.length >= BATCH_MAX_RECORDS || bytes + size > BATCH_MAX_BYTES) break;
					batch.push(record);
					bytes += size;
				}
				if (batch.length === 0) {
					await removeRecords(agentDir, new Set([pending[0].recordId]), endpoint.url);
					continue;
				}
				const outcome = await sendBatch(endpoint.url, access, batch, endpoint.ca);
				if (outcome.kind === "accepted" || outcome.kind === "bad")
					await removeRecords(agentDir, new Set(batch.map((record) => record.recordId)), endpoint.url);
				if (outcome.kind === "stop") state.stopped = true;
				if (outcome.kind !== "accepted") return;
			}
		} while (state.rerun);
	} catch {
		// 单端点故障保留本地数据，不传播到其他端点或诊断。
	} finally {
		state.inflight = false;
	}
}
