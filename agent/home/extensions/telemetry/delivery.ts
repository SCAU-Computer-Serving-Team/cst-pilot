import type { TelemetryEndpoint } from "./config.ts";
import { diagnose } from "./diagnostics.ts";
import { appendRecord, BATCH_MAX_RECORDS, readAll, removeRecords } from "./outbox.ts";
import type { SessionRecord } from "./record.ts";
import { shared } from "./shared.ts";
import { sendBatch, type UploadAuth } from "./transport.ts";

const BATCH_MAX_BYTES = 900 * 1024;

/** 同一 recordId 写入全部目标；一端确认不会删除另一端的队列。 */
export async function queueRecord(
	agentDir: string,
	record: SessionRecord,
	endpoints: TelemetryEndpoint[],
): Promise<void> {
	const results = await Promise.allSettled(
		endpoints.map(async (endpoint) => {
			const queued = await appendRecord(agentDir, record, endpoint.url);
			if (!queued) throw new Error("遥测记录超过队列大小限制");
			await diagnose(agentDir, {
				endpoint: endpoint.url,
				queued: { recordId: queued.recordId, revision: queued.revision, final: queued.final },
			});
		}),
	);
	if (results.some((result) => result.status === "rejected")) {
		await diagnose(agentDir, { failure: "storage" });
		throw new Error("遥测队列写入失败");
	}
}

/** 先复制到所有目标，再清除单端点队列；中断重试使用 recordId 去重。 */
export async function migratePending(agentDir: string, endpoints: TelemetryEndpoint[]): Promise<void> {
	if (endpoints.length === 0) return;
	const pending = await readAll(agentDir);
	for (const record of pending) await queueRecord(agentDir, record, endpoints);
	await removeRecords(agentDir, pending);
}

export async function flushEndpoints(
	agentDir: string,
	access: UploadAuth,
	endpoints: TelemetryEndpoint[],
): Promise<void> {
	await Promise.allSettled(endpoints.map((endpoint) => flushEndpoint(agentDir, access, endpoint)));
}

async function flushEndpoint(agentDir: string, access: UploadAuth, endpoint: TelemetryEndpoint): Promise<void> {
	if (endpoint.unavailable) {
		await diagnose(agentDir, { endpoint: endpoint.url, failure: "tls" });
		return;
	}
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
					if (record.v !== pending[0].v) break;
					const size = Buffer.byteLength(JSON.stringify(record), "utf8");
					if (batch.length >= BATCH_MAX_RECORDS || bytes + size > BATCH_MAX_BYTES) break;
					batch.push(record);
					bytes += size;
				}
				if (batch.length === 0) {
					await removeRecords(agentDir, [pending[0]], endpoint.url);
					continue;
				}
				const outcome = await sendBatch(endpoint.url, access, batch, endpoint.ca);
				if (outcome.kind === "accepted") {
					const acknowledged = outcome.acknowledged ?? batch;
					await removeRecords(agentDir, acknowledged, endpoint.url);
					await diagnose(agentDir, {
						endpoint: endpoint.url,
						acks: acknowledged.map((record) => ({
							recordId: record.recordId,
							revision: record.revision,
							status: outcome.rejected?.includes(record) ? "rejected" : "confirmed",
						})),
					});
					if (acknowledged.length < batch.length) {
						await diagnose(agentDir, { endpoint: endpoint.url, failure: "protocol" });
						return;
					}
				} else if (outcome.kind === "bad") await removeRecords(agentDir, batch, endpoint.url);
				if (outcome.kind !== "accepted")
					await diagnose(agentDir, {
						endpoint: endpoint.url,
						failure: outcome.failure ?? (outcome.kind === "auth" ? "auth" : "http"),
					});
				if (outcome.kind === "stop") state.stopped = true;
				if (outcome.kind !== "accepted") return;
			}
		} while (state.rerun);
	} catch {
		await diagnose(agentDir, { endpoint: endpoint.url, failure: "storage" });
	} finally {
		state.inflight = false;
	}
}
