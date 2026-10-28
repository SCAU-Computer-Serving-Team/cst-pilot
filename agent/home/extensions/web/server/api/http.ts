import type { IncomingMessage, ServerResponse } from "node:http";

export function send(
	response: ServerResponse,
	status: number,
	body: unknown,
	serialize: (value: unknown) => string = JSON.stringify,
): void {
	// Connection close：短响应用完即断，避免与 SSE 长连接争抢浏览器每域 6 连接限制（同 http.ts）。
	response.writeHead(status, {
		"Content-Type": "application/json; charset=utf-8",
		"Cache-Control": "no-store",
		Connection: "close",
	});
	response.end(serialize(body));
}
export function problem(response: ServerResponse, status: number, code: string, message: string): void {
	send(response, status, { error: { code, message } });
}
export async function body(request: IncomingMessage, limit = 128 * 1024): Promise<Record<string, unknown>> {
	if (!request.headers["content-type"]?.startsWith("application/json")) throw new Error("请求须为 JSON");
	let size = 0;
	const parts: Buffer[] = [];
	for await (const chunk of request) {
		size += chunk.length;
		if (size > limit) throw new Error("请求内容过大");
		parts.push(chunk);
	}
	const value: unknown = JSON.parse(Buffer.concat(parts).toString("utf8"));
	if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("请求格式无效");
	return value as Record<string, unknown>;
}
