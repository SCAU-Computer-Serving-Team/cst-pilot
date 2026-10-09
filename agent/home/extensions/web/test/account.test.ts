import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import type { ModelRuntime } from "@earendil-works/pi-coding-agent";
import { getCstoaProfile } from "../server/api/account.ts";

// 本地联调用的源站覆盖会改变解析目标，测试固定走 runtime 里的 baseUrl。
delete process.env.CSTOA_OA_HOST;

const BASE = "http://127.0.0.1:8788/v1";
const ENDPOINT = "http://127.0.0.1:8788/api/agent/profile";

function jwt(payload: Record<string, unknown>): string {
	return `${Buffer.from(JSON.stringify(payload), "utf8").toString("base64url")}.signature`;
}

function liveToken(mid = "202333210102"): string {
	return jwt({ mid, typ: "agent", exp: Math.floor(Date.now() / 1000) + 3600, jti: Math.random().toString(36) });
}

function runtime(baseUrl: string, key?: string): ModelRuntime {
	return {
		getModels: () => [{ provider: "cstoa", baseUrl }],
		getAuth: async () => (key === undefined ? undefined : { auth: { apiKey: key } }),
	} as unknown as ModelRuntime;
}

/** pi 在没有刷新令牌时会拒绝取用过期凭据。 */
function failingRuntime(baseUrl: string): ModelRuntime {
	return {
		getModels: () => [{ provider: "cstoa", baseUrl }],
		getAuth: async () => {
			throw new Error("OAuth refresh failed for cstoa: 没有可用的刷新令牌，请重新登录。");
		},
	} as unknown as ModelRuntime;
}

async function withAuthFile(access: string, run: (dir: string) => Promise<void>) {
	const dir = await mkdtemp(join(tmpdir(), "cst-account-"));
	try {
		await writeFile(
			join(dir, "auth.json"),
			JSON.stringify({ cstoa: { access, refresh: "", expires: Date.now() + 3600_000, type: "oauth" } }),
		);
		await run(dir);
	} finally {
		await rm(dir, { recursive: true, force: true });
	}
}

test("资料齐全时返回学号、姓名与剩余额度点，并按令牌缓存", async () => {
	const token = liveToken();
	const calls: string[] = [];
	const fetcher = async (url: URL | RequestInfo, options?: RequestInit) => {
		calls.push(String(url));
		assert.equal(new Headers(options?.headers).get("Authorization"), `Bearer ${token}`);
		assert.equal(options?.redirect, "error");
		return Response.json({
			result: true,
			data: {
				memberId: "202333210102",
				name: "陈佳庆",
				groupName: "管理员",
				balance: 12345,
				usedQuota: 500,
				requestCount: 3,
				balanceAvailable: true,
			},
		});
	};
	const result = await getCstoaProfile("cstoa", runtime(BASE, token), fetcher as typeof fetch);
	assert.equal(result.supported, true);
	assert.equal(result.studentId, "202333210102");
	assert.equal(result.name, "陈佳庆");
	assert.equal(result.balance, 12345);
	assert.equal(result.balanceSupported, true);
	assert.equal(result.expired, false);
	assert.deepEqual(calls, [ENDPOINT]);
	await getCstoaProfile("cstoa", runtime(BASE, token), fetcher as typeof fetch);
	assert.equal(calls.length, 1);
});

test("OA 没有资料接口时只保留学号，不伪造姓名与额度", async () => {
	const result = await getCstoaProfile(
		"cstoa",
		runtime(BASE, liveToken()),
		(async () => new Response(null, { status: 404 })) as typeof fetch,
	);
	assert.equal(result.supported, true);
	assert.equal(result.studentId, "202333210102");
	assert.equal(result.name, null);
	assert.equal(result.balance, null);
	assert.equal(result.balanceSupported, false);
	assert.equal(result.reason, "资料接口返回 404");
});

test("OA 返回业务失败时不伪造资料", async () => {
	const result = await getCstoaProfile("cstoa", runtime(BASE, liveToken()), (async () =>
		Response.json({ result: false, code: 19008, reason: "Agent 凭据无效或已过期", data: {} })) as typeof fetch);
	assert.equal(result.studentId, "202333210102");
	assert.equal(result.name, null);
	assert.equal(result.balance, null);
	assert.equal(result.reason, "Agent 凭据无效或已过期");
});

test("额度字段缺失时姓名照常返回", async () => {
	const result = await getCstoaProfile("cstoa", runtime(BASE, liveToken()), (async () =>
		Response.json({ result: true, data: { name: "陈佳庆" } })) as typeof fetch);
	assert.equal(result.name, "陈佳庆");
	assert.equal(result.balance, null);
	assert.equal(result.balanceSupported, false);
});

test("凭据过期时不请求 OA，只提示重新登录", async () => {
	const expired = jwt({ mid: "202333210102", exp: Math.floor(Date.now() / 1000) - 10 });
	let calls = 0;
	const result = await getCstoaProfile("cstoa", runtime(BASE, expired), (async () => {
		calls++;
		return Response.json({});
	}) as typeof fetch);
	assert.equal(calls, 0);
	assert.equal(result.supported, true);
	assert.equal(result.expired, true);
	assert.equal(result.studentId, "202333210102");
	assert.match(result.reason ?? "", /重新登录/);
});

test("缺少凭据、来源不可信或未配置时不联网", async () => {
	let calls = 0;
	const fetcher = (async () => {
		calls++;
		return Response.json({});
	}) as typeof fetch;

	const noKey = await getCstoaProfile("cstoa", runtime(BASE), fetcher);
	assert.equal(noKey.supported, false);
	assert.equal(noKey.reason, "缺少凭据");

	const untrusted = await getCstoaProfile("cstoa", runtime("http://example.com/v1", liveToken()), fetcher);
	assert.equal(untrusted.supported, false);
	assert.equal(untrusted.reason, "OA 地址无效");

	const noModel = await getCstoaProfile(
		"cstoa",
		{ getModels: () => [], getAuth: async () => undefined } as unknown as ModelRuntime,
		fetcher,
	);
	assert.equal(noModel.supported, false);
	assert.equal(noModel.reason, "该模型服务未配置");
	assert.equal(calls, 0);
});

test("令牌载荷不可解析时不抛错，仍按 OA 响应取值", async () => {
	const result = await getCstoaProfile("cstoa", runtime(BASE, "not-a-jwt"), (async () =>
		Response.json({ result: true, data: { name: "陈佳庆", balance: 5, balanceAvailable: true } })) as typeof fetch);
	assert.equal(result.supported, true);
	assert.equal(result.studentId, null);
	assert.equal(result.name, "陈佳庆");
	assert.equal(result.balance, 5);
});

test("网络失败时给出可展示的原因", async () => {
	const result = await getCstoaProfile("cstoa", runtime(BASE, liveToken()), (async () => {
		throw new Error("offline");
	}) as typeof fetch);
	assert.equal(result.reason, "资料查询失败");
	assert.equal(result.studentId, "202333210102");
});

test("getAuth 刷新失败时读本地已过期令牌，提示重新登录且不联网", async () => {
	const token = jwt({ mid: "202333210102", exp: Math.floor(Date.now() / 1000) - 10 });
	await withAuthFile(token, async (dir) => {
		let calls = 0;
		const result = await getCstoaProfile(
			"cstoa",
			failingRuntime(BASE),
			(async () => {
				calls++;
				return Response.json({});
			}) as typeof fetch,
			dir,
		);
		assert.equal(calls, 0);
		assert.equal(result.supported, true);
		assert.equal(result.studentId, "202333210102");
		assert.equal(result.expired, true);
	});
});

test("getAuth 刷新失败但本地令牌仍有效时继续查询资料", async () => {
	const token = liveToken();
	await withAuthFile(token, async (dir) => {
		const result = await getCstoaProfile(
			"cstoa",
			failingRuntime(BASE),
			(async (_url, options) => {
				assert.equal(new Headers(options?.headers).get("Authorization"), `Bearer ${token}`);
				return Response.json({
					result: true,
					data: { memberId: "202333210102", name: "陈佳庆", balance: 7, balanceAvailable: true },
				});
			}) as typeof fetch,
			dir,
		);
		assert.equal(result.name, "陈佳庆");
		assert.equal(result.balance, 7);
	});
});
