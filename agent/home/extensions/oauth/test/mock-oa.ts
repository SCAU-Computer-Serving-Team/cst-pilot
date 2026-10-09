/**
 * 本地 mock 的 OA 设备授权服务，供 oauth 单测与 e2e 共用。
 */

import { createServer, type Server } from "node:http";

export interface MockOptions {
	/** 前 N 次轮询返回 authorization_pending。 */
	pendingTimes?: number;
	/** 第一次轮询返回 slow_down。 */
	slowDownOnce?: boolean;
	/** 授权结果；默认 approve。 */
	outcome?: "approve" | "deny" | "expire";
	/** 账号接口状态，可在浏览器测试中切换以覆盖重试。 */
	profileStatus?: number;
}

export interface MockOa {
	host: string;
	requests: { path: string; body: Record<string, unknown> }[];
	close(): Promise<void>;
}

export async function startMockOa(options: MockOptions = {}): Promise<MockOa> {
	let polls = 0;
	let host = "";
	const requests: MockOa["requests"] = [];
	const server: Server = createServer(async (request, response) => {
		let raw = "";
		for await (const chunk of request) raw += chunk;
		const body = raw ? (JSON.parse(raw) as Record<string, unknown>) : {};
		requests.push({ path: request.url ?? "", body });
		response.setHeader("Content-Type", "application/json");

		if (request.url === "/api/oauth/device_authorization") {
			response.end(
				JSON.stringify({
					device_code: "device-code-1",
					user_code: "123456",
					verification_uri: `${host}/oauth/device`,
					verification_uri_complete: `${host}/#/oauth/device?code=123456`,
					expires_in: 60,
					interval: 1,
				}),
			);
			return;
		}
		if (request.url === "/api/agent/me") {
			if (options.profileStatus && options.profileStatus !== 200) {
				response.statusCode = options.profileStatus;
				response.end("{}");
				return;
			}
			if (!["Bearer access-1", "Bearer access-2"].includes(request.headers.authorization ?? "")) {
				response.statusCode = 401;
				response.end("{}");
				return;
			}
			response.end(JSON.stringify({ result: true, data: { id: "20230001", name: "测试队员" } }));
			return;
		}
		if (request.url === "/api/oauth/token") {
			if (body.grant_type === "refresh_token") {
				if (body.refresh_token === "refresh-1") {
					response.end(
						JSON.stringify({
							access_token: "access-2",
							refresh_token: "refresh-2",
							token_type: "Bearer",
							expires_in: 7200,
						}),
					);
				} else {
					response.statusCode = 400;
					response.end(JSON.stringify({ error: "invalid_grant" }));
				}
				return;
			}
			polls += 1;
			const outcome = options.outcome ?? "approve";
			if (outcome === "deny") {
				response.statusCode = 400;
				response.end(JSON.stringify({ error: "access_denied" }));
				return;
			}
			if (outcome === "expire") {
				response.statusCode = 400;
				response.end(JSON.stringify({ error: "expired_token" }));
				return;
			}
			if (polls <= (options.pendingTimes ?? 0)) {
				response.end(JSON.stringify({ error: "authorization_pending" }));
				return;
			}
			if (options.slowDownOnce && polls === 1) {
				response.end(JSON.stringify({ error: "slow_down" }));
				return;
			}
			response.end(
				JSON.stringify({
					access_token: "access-1",
					refresh_token: "refresh-1",
					token_type: "Bearer",
					expires_in: 7200,
				}),
			);
			return;
		}
		response.statusCode = 404;
		response.end("{}");
	});
	await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
	const address = server.address();
	if (!address || typeof address === "string") throw new Error("mock 服务启动失败");
	host = `http://127.0.0.1:${address.port}`;
	return {
		host,
		requests,
		close: () =>
			new Promise<void>((resolve, reject) => {
				// undici 的 keep-alive 连接不主动断开，会拖住 server.close。
				server.closeAllConnections();
				server.close((error) => (error ? reject(error) : resolve()));
			}),
	};
}
