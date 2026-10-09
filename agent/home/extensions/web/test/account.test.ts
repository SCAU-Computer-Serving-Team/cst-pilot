import assert from "node:assert/strict";
import { test } from "node:test";
import type { ModelRuntime } from "@earendil-works/pi-coding-agent";
import { getCstoaAccount } from "../server/api/account.ts";

function runtime(signedIn = true, access = "test-agent-token"): Pick<ModelRuntime, "listCredentials" | "getAuth"> {
	return {
		listCredentials: async () => (signedIn ? [{ providerId: "cstoa", type: "oauth" }] : []),
		getAuth: async () => ({ auth: { apiKey: access } }),
	} as unknown as Pick<ModelRuntime, "listCredentials" | "getAuth">;
}

test("账号查询复用 Agent 凭据，只向浏览器返回学号姓名", async () => {
	const result = await getCstoaAccount(runtime(), false, async (url, options) => {
		assert.equal(String(url), "https://www.cstoa.top/api/agent/me");
		assert.equal(new Headers(options?.headers).get("Authorization"), "Bearer test-agent-token");
		assert.equal(options?.redirect, "error");
		assert.ok(options?.signal);
		return Response.json({ result: true, data: { id: "20230001", name: "测试队员", phone: "private" } });
	});
	assert.deepEqual(result.profile, { supported: true, studentId: "20230001", name: "测试队员" });
	assert.equal(result.signedIn, true);
	assert.equal(result.requiresLogin, false);
	assert.equal(JSON.stringify(result).includes("test-agent-token"), false);
	assert.equal(JSON.stringify(result).includes("private"), false);
});

test("未登录、明确失效或无访问令牌时不查询资料", async () => {
	let calls = 0;
	const fetcher: typeof fetch = async () => {
		calls++;
		throw new Error("不应联网");
	};
	assert.equal((await getCstoaAccount(runtime(false), false, fetcher)).signedIn, false);
	assert.equal((await getCstoaAccount(runtime(), true, fetcher)).requiresLogin, true);
	assert.equal((await getCstoaAccount(runtime(true, ""), false, fetcher)).requiresLogin, true);
	assert.equal(calls, 0);
});

test("HTTP 401/403 与 OA 19008 提示重新登录", async () => {
	for (const status of [401, 403]) {
		const result = await getCstoaAccount(runtime(), false, async () => new Response(null, { status }));
		assert.equal(result.requiresLogin, true);
		assert.equal(result.profile.name, null);
	}
	const result = await getCstoaAccount(runtime(), false, async () =>
		Response.json({ result: false, code: 19008, reason: "Agent 凭据无效或已过期" }),
	);
	assert.equal(result.requiresLogin, true);
});

test("接口未上线、服务错误和网络故障保留退出能力，不伪造姓名", async () => {
	for (const status of [404, 405, 500]) {
		const result = await getCstoaAccount(runtime(), false, async () => new Response("test-agent-token", { status }));
		assert.equal(result.signedIn, true);
		assert.equal(result.requiresLogin, false);
		assert.equal(result.profile.supported, status === 500);
		assert.equal(result.profile.studentId, null);
		assert.ok(result.profile.reason);
		assert.equal(JSON.stringify(result).includes("test-agent-token"), false);
	}
	const result = await getCstoaAccount(runtime(), false, async () => {
		throw new Error("test-agent-token");
	});
	assert.ok(result.profile.reason);
	assert.equal(result.requiresLogin, false);
	assert.equal(JSON.stringify(result).includes("test-agent-token"), false);
});

test("无效资料或业务错误不使用上一次账号的数据", async () => {
	for (const payload of [
		{ result: true, data: { id: "", name: "姓名" } },
		{ result: true, data: { id: "20230001", name: 123 } },
		{ result: false, code: 10005 },
		{ data: { id: "20230001", name: "姓名" } },
	]) {
		const result = await getCstoaAccount(runtime(), false, async () => Response.json(payload));
		assert.equal(result.profile.studentId, null);
		assert.equal(result.profile.name, null);
		assert.ok(result.profile.reason);
	}
});
