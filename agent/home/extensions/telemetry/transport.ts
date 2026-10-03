/**
 * 传输：批量 POST 上传信封，超时 5 秒，响应按契约分类。
 * 网络层失败不抛错，按 retry 分类返回。
 */

import type { SessionRecord } from "./record.ts";

export const POST_TIMEOUT_MS = 5000;

export type SendOutcome =
	| { kind: "accepted" }
	| { kind: "auth" } // 401 / 403：留队列，等重新登录
	| { kind: "bad" } // 400 / 413：这批本身不合法，丢弃
	| { kind: "stop" } // 404 / 410：端点停采，停发不删
	| { kind: "retry" }; // 429 / 5xx / 网络错误 / 超时：留队列下次再发

export async function sendBatch(endpoint: string, accessToken: string, records: SessionRecord[]): Promise<SendOutcome> {
	const body = JSON.stringify({
		v: records[0]?.v ?? "0.1",
		batchId: crypto.randomUUID(),
		sentAt: new Date().toISOString(),
		records,
	});
	const controller = new AbortController();
	const timer = setTimeout(() => controller.abort(), POST_TIMEOUT_MS);
	try {
		const response = await fetch(endpoint, {
			method: "POST",
			headers: {
				"Content-Type": "application/json",
				Authorization: `Bearer ${accessToken}`,
			},
			body,
			signal: controller.signal,
		});
		if (response.status === 202) return { kind: "accepted" };
		if (response.status === 401 || response.status === 403) return { kind: "auth" };
		if (response.status === 400 || response.status === 413) return { kind: "bad" };
		if (response.status === 404 || response.status === 410) return { kind: "stop" };
		return { kind: "retry" };
	} catch {
		return { kind: "retry" };
	} finally {
		clearTimeout(timer);
	}
}
