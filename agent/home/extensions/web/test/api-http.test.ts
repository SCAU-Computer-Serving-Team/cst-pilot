import assert from "node:assert/strict";
import { createServer, type RequestListener } from "node:http";
import { type TestContext, test } from "node:test";
import { BitLockerExpiredError, BitLockerInputError } from "../../diagnostics/bitlocker-web.ts";
import { SessionOwnerConflict } from "../../runtime/owner.ts";
import { sendError } from "../server/api/errors.ts";
import { body, send } from "../server/api/http.ts";
import { RunbookOpenError } from "../server/api/runbook.ts";
import { InboxConflict } from "../server/session/inbox.ts";

async function listen(t: TestContext, handler: RequestListener): Promise<string> {
	const server = createServer(handler);
	t.after(async () => {
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	});
	await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
	const address = server.address();
	assert.ok(address && typeof address !== "string");
	return `http://127.0.0.1:${address.port}`;
}

test("JSON 请求共用对象校验、字节上限与无缓存响应", async (t) => {
	const origin = await listen(t, (request, response) => {
		void body(request, 32)
			.then((value) => send(response, 200, value))
			.catch((error: unknown) => sendError(response, error));
	});
	const cases = [
		{ raw: '{"text":"中文"}', status: 200 },
		{ raw: "null", status: 400 },
		{ raw: "[]", status: 400 },
		{ raw: "1", status: 400 },
		{ raw: "{", status: 400, code: "invalid_json" },
		{ raw: JSON.stringify({ text: "中".repeat(10) }), status: 400 },
		{ raw: "{}", contentType: "text/plain", status: 400 },
	];
	for (const { raw, contentType = "application/json", status, code } of cases) {
		const response = await fetch(origin, {
			method: "POST",
			headers: { "Content-Type": contentType },
			body: raw,
		});
		assert.equal(response.status, status, raw);
		assert.equal(response.headers.get("cache-control"), "no-store");
		assert.equal(response.headers.get("connection"), "close");
		assert.match(response.headers.get("content-type") ?? "", /application\/json/);
		const value = await response.json();
		if (status === 200) assert.deepEqual(value, JSON.parse(raw));
		else assert.equal(value.error.code, code ?? "invalid_request", raw);
	}
});

test("统一错误映射保留状态码，并安全处理任意抛出值", async (t) => {
	const cases: { error: unknown; status: number; code: string }[] = [
		{ error: null, status: 500, code: "server_error" },
		{ error: undefined, status: 500, code: "server_error" },
		{ error: "secret-path-and-token", status: 500, code: "server_error" },
		{ error: new Error("secret-path-and-token"), status: 500, code: "server_error" },
		{ error: { code: 42 }, status: 500, code: "server_error" },
		{ error: { code: "ENOSPC" }, status: 507, code: "storage_full" },
		{ error: new SyntaxError("secret-path-and-token"), status: 400, code: "invalid_json" },
		{ error: new Error("请求格式无效"), status: 400, code: "invalid_request" },
		{ error: new Error("会话不存在"), status: 404, code: "not_found" },
		{ error: new Error("会话已删除"), status: 409, code: "conflict" },
		{ error: new Error("Already compacted"), status: 409, code: "already_compacted" },
		{ error: new Error("Nothing to compact (session too small)"), status: 409, code: "nothing_to_compact" },
		{ error: new InboxConflict("队列版本过期"), status: 409, code: "conflict" },
		{ error: new SessionOwnerConflict(), status: 409, code: "session_owned" },
		{ error: { code: "CST_SESSION_OWNED" }, status: 409, code: "session_owned" },
		{ error: new BitLockerInputError("输入无效"), status: 400, code: "invalid_bitlocker_input" },
		{ error: new BitLockerExpiredError("请求已失效"), status: 409, code: "expired_request" },
		{ error: new RunbookOpenError(404, "runbook_missing", "文件不存在"), status: 404, code: "runbook_missing" },
		{
			error: Object.assign(new Error("配置暂不可用"), { code: "CST_CONFIGURATION_UNAVAILABLE" }),
			status: 503,
			code: "configuration_unavailable",
		},
		...["EACCES", "EPERM", "EROFS", "ENOTDIR", "EEXIST"].map((code) => ({
			error: { code },
			status: 500,
			code: "storage_unavailable",
		})),
	];
	const origin = await listen(t, (request, response) => {
		sendError(response, cases[Number(request.url?.slice(1))]?.error);
	});
	for (const [index, { status, code }] of cases.entries()) {
		const response = await fetch(`${origin}/${index}`);
		assert.equal(response.status, status, String(index));
		const raw = await response.text();
		assert.equal(JSON.parse(raw).error.code, code, String(index));
		assert.equal(raw.includes("secret-path-and-token"), false);
	}
});
