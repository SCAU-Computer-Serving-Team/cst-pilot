import { request as httpsRequest } from "node:https";
import type { FailureKind } from "./diagnostics.ts";
import type { Heartbeat } from "./heartbeat.ts";
import { type Installation, installationHeaders } from "./installation.ts";
import type { SessionRecord } from "./record.ts";

export type UploadAuth = string | Installation | { installation: Installation; access: string };

export const POST_TIMEOUT_MS = 5000;
const RESPONSE_MAX_BYTES = 64 * 1024;
export type SendOutcome =
	| { kind: "accepted"; acknowledged?: SessionRecord[]; rejected?: SessionRecord[]; heartbeatAcknowledged?: boolean }
	| { kind: "auth" | "bad" | "stop" | "retry"; failure?: FailureKind };

function outcome(status: number): SendOutcome {
	if (status === 202) return { kind: "accepted" };
	if (status === 401 || status === 403) return { kind: "auth", failure: "auth" };
	if (status === 400 || status === 413) return { kind: "bad", failure: "http" };
	if (status === 404 || status === 410) return { kind: "stop", failure: "http" };
	return { kind: "retry", failure: "http" };
}

function object(value: unknown): Record<string, unknown> | undefined {
	return value !== null && typeof value === "object" && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: undefined;
}

/** 专用 CA 仅传给本次请求；两个路径均限制响应体积且禁止重定向。 */
async function request(
	endpoint: string,
	method: "GET" | "POST",
	headers: Record<string, string>,
	body: string | undefined,
	signal: AbortSignal,
	ca?: string,
): Promise<{ status: number; body: string }> {
	if (ca) {
		if (new URL(endpoint).protocol !== "https:") throw new Error("CA 需要 HTTPS");
		return new Promise((resolve, reject) => {
			const req = httpsRequest(
				endpoint,
				{ method, headers, ca, rejectUnauthorized: true, signal, agent: false },
				(res) => {
					const chunks: Buffer[] = [];
					let bytes = 0;
					res.on("data", (chunk: Buffer) => {
						bytes += chunk.length;
						if (bytes > RESPONSE_MAX_BYTES) res.destroy(new Error("遥测响应过大"));
						else chunks.push(chunk);
					});
					res.on("error", reject);
					res.on("end", () =>
						resolve({ status: res.statusCode ?? 0, body: Buffer.concat(chunks).toString("utf8") }),
					);
				},
			);
			req.on("error", reject);
			req.end(body);
		});
	}
	const response = await fetch(endpoint, { method, headers, body, signal, redirect: "error" });
	const reader = response.body?.getReader();
	const chunks: Buffer[] = [];
	let bytes = 0;
	try {
		if (reader) {
			for (;;) {
				const chunk = await reader.read();
				if (chunk.done) break;
				bytes += chunk.value.byteLength;
				if (bytes > RESPONSE_MAX_BYTES) throw new Error("遥测响应过大");
				chunks.push(Buffer.from(chunk.value));
			}
		}
	} finally {
		await reader?.cancel();
	}
	return { status: response.status, body: Buffer.concat(chunks).toString("utf8") };
}

function acknowledgments(raw: string, records: SessionRecord[], rejectedOnly = false): SessionRecord[] {
	const parsed = object(JSON.parse(raw));
	if (parsed?.v !== "0.2" || !Array.isArray(parsed.acks)) return [];
	const acks = parsed.acks;
	return records.filter((record) => {
		const matches = acks.filter((entry: unknown) => {
			const ack = object(entry);
			return ack?.recordId === record.recordId && ack?.revision === record.revision;
		});
		if (matches.length !== 1) return false;
		const ack = object(matches[0]);
		if (["stored", "duplicate", "stale"].includes(String(ack?.status))) return !rejectedOnly;
		return (
			ack?.status === "rejected" &&
			typeof ack.reason === "string" &&
			[
				"bad-revision",
				"bad-final-state",
				"bad-record-id",
				"unknown-type",
				"too-many-elements",
				"installation-mismatch",
				"immutable-legacy-record",
				"owner-mismatch",
				"instance-mismatch",
				"already-final",
				"envelope-version-mismatch",
			].includes(ack.reason)
		);
	});
}

export async function sendBatch(
	endpoint: string,
	accessToken: UploadAuth,
	records: SessionRecord[],
	ca?: string,
	heartbeat?: Heartbeat,
): Promise<SendOutcome> {
	const snapshots = records[0]?.v === "0.2" || heartbeat !== undefined;
	const controller = new AbortController();
	const timer = setTimeout(() => controller.abort(), POST_TIMEOUT_MS);
	try {
		if (snapshots) {
			const response = await request(endpoint, "GET", {}, undefined, controller.signal, ca);
			if (response.status !== 200) return { kind: "retry", failure: "capability" };
			const capability = object(JSON.parse(response.body));
			if (
				!Array.isArray(capability?.protocols) ||
				!capability.protocols.includes("0.2") ||
				!Array.isArray(capability?.capabilities) ||
				!capability.capabilities.includes("session-snapshots") ||
				!capability.capabilities.includes("revision-acks") ||
				(heartbeat !== undefined && !capability.capabilities.includes("heartbeat"))
			)
				return { kind: "retry", failure: "capability" };
			if (typeof accessToken === "string") return { kind: "auth", failure: "auth" };
		}
		const body = JSON.stringify({
			v: snapshots ? "0.2" : (records[0]?.v ?? "0.1"),
			batchId: crypto.randomUUID(),
			sentAt: new Date().toISOString(),
			records,
			...(heartbeat ? { heartbeat } : {}),
		});
		let authHeaders: Record<string, string>;
		if (typeof accessToken === "string") authHeaders = { Authorization: `Bearer ${accessToken}` };
		else if ("installation" in accessToken)
			authHeaders = {
				...installationHeaders(accessToken.installation, body),
				Authorization: `Bearer ${accessToken.access}`,
				"X-Telemetry-Installation": accessToken.installation.installationId,
			};
		else authHeaders = installationHeaders(accessToken, body);
		const response = await request(
			endpoint,
			"POST",
			{ "Content-Type": "application/json", ...authHeaders },
			body,
			controller.signal,
			ca,
		);
		const result = outcome(response.status);
		if (!snapshots) return result;
		if (result.kind === "accepted") {
			const parsed = object(JSON.parse(response.body));
			const ack = object(parsed?.heartbeatAck);
			return {
				kind: "accepted",
				acknowledged: acknowledgments(response.body, records),
				rejected: acknowledgments(response.body, records, true),
				...(heartbeat
					? {
							heartbeatAcknowledged:
								parsed?.v === "0.2" &&
								ack?.runId === heartbeat.runId &&
								ack?.sequence === heartbeat.sequence &&
								["stored", "duplicate", "stale"].includes(String(ack?.status)),
						}
					: {}),
			};
		}
		// 未得到新协议逐项确认时保留快照，兼容接收端回退。
		return result.kind === "bad" ? { kind: "retry", failure: "http" } : result;
	} catch (error) {
		const code =
			error instanceof Error
				? String(
						(error as NodeJS.ErrnoException).code ??
							(error.cause as NodeJS.ErrnoException | undefined)?.code ??
							"",
					)
				: "";
		return {
			kind: "retry",
			failure: controller.signal.aborted
				? "timeout"
				: error instanceof SyntaxError
					? "protocol"
					: /CERT|TLS|SSL/.test(code)
						? "tls"
						: "network",
		};
	} finally {
		clearTimeout(timer);
	}
}
