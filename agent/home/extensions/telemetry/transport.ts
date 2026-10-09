import { request as httpsRequest } from "node:https";
import type { SessionRecord } from "./record.ts";

export const POST_TIMEOUT_MS = 5000;
export type SendOutcome = { kind: "accepted" | "auth" | "bad" | "stop" | "retry" };

function outcome(status: number): SendOutcome {
	if (status === 202) return { kind: "accepted" };
	if (status === 401 || status === 403) return { kind: "auth" };
	if (status === 400 || status === 413) return { kind: "bad" };
	if (status === 404 || status === 410) return { kind: "stop" };
	return { kind: "retry" };
}

/** 专用 CA 仅传给本次 HTTPS 请求，仍校验证书链、有效期与主机名。 */
export async function sendBatch(
	endpoint: string,
	accessToken: string,
	records: SessionRecord[],
	ca?: string,
): Promise<SendOutcome> {
	const body = JSON.stringify({
		v: records[0]?.v ?? "0.1",
		batchId: crypto.randomUUID(),
		sentAt: new Date().toISOString(),
		records,
	});
	const controller = new AbortController();
	const timer = setTimeout(() => controller.abort(), POST_TIMEOUT_MS);
	const headers = { "Content-Type": "application/json", Authorization: `Bearer ${accessToken}` };
	try {
		if (ca) {
			if (new URL(endpoint).protocol !== "https:") return { kind: "retry" };
			return await new Promise<SendOutcome>((resolve) => {
				const request = httpsRequest(
					endpoint,
					{
						method: "POST",
						headers,
						ca,
						rejectUnauthorized: true,
						signal: controller.signal,
						agent: false,
					},
					(response) => {
						const result = outcome(response.statusCode ?? 0);
						response.destroy();
						resolve(result);
					},
				);
				request.on("error", () => resolve({ kind: "retry" }));
				request.end(body);
			});
		}
		const response = await fetch(endpoint, {
			method: "POST",
			headers,
			body,
			signal: controller.signal,
			redirect: "error",
		});
		await response.body?.cancel();
		return outcome(response.status);
	} catch {
		return { kind: "retry" };
	} finally {
		clearTimeout(timer);
	}
}
