import assert from "node:assert/strict";
import { after, test } from "node:test";
import type { ModelRuntime } from "@earendil-works/pi-coding-agent";
import { CstoaLoginRequired } from "../../oauth/errors.ts";
import { CstoaAccount } from "../server/api/account.ts";

type Runtime = Pick<ModelRuntime, "listCredentials" | "getAuth">;
const previousHost = process.env.CSTOA_OA_HOST;
delete process.env.CSTOA_OA_HOST;
after(() => {
	if (previousHost === undefined) delete process.env.CSTOA_OA_HOST;
	else process.env.CSTOA_OA_HOST = previousHost;
});
function deferred<T>() {
	let resolve!: (value: T) => void;
	const promise = new Promise<T>((done) => {
		resolve = done;
	});
	return { promise, resolve };
}

function runtime(signedIn = true, access = "test-agent-token"): Runtime {
	return {
		listCredentials: async () => (signedIn ? [{ providerId: "cstoa", type: "oauth" }] : []),
		getAuth: async () => ({ auth: { apiKey: access } }),
	};
}
const profile = (patch: Record<string, unknown> = {}) =>
	Response.json({
		result: true,
		data: { memberId: "20230001", name: "测试队员", balance: 12345, balanceAvailable: true, ...patch },
	});

test("账号与额度复用 OA 查询，身份来自服务端，响应不含令牌或其他个人字段", async () => {
	let calls = 0;
	const service = new CstoaAccount(async (url, options) => {
		calls++;
		assert.equal(String(url), "https://www.cstoa.top/api/agent/profile");
		assert.equal(new Headers(options?.headers).get("Authorization"), "Bearer test-agent-token");
		assert.equal(options?.redirect, "error");
		assert.ok(options?.signal);
		return profile({ phone: "private", groupName: "private", usedQuota: 99 });
	});
	const [account, quota] = await Promise.all([service.getAccount(runtime()), service.getQuota(runtime())]);
	assert.equal(calls, 1);
	assert.deepEqual(account.profile, { supported: true, studentId: "20230001", name: "测试队员" });
	assert.equal("quota" in account, false);
	assert.equal(account.signedIn, true);
	assert.equal(account.requiresLogin, false);
	assert.equal(quota.balance, 12345);
	assert.equal(quota.unit, "credit");
	assert.equal(JSON.stringify([account, quota]).includes("test-agent-token"), false);
	assert.equal(JSON.stringify([account, quota]).includes("private"), false);
});

test("零额度有效；额度不可用或格式错误仍返回姓名，不伪造零值", async () => {
	const zero = await new CstoaAccount(async () => profile({ balance: 0 })).getQuota(runtime());
	assert.equal(zero.supported, true);
	assert.equal(zero.balance, 0);
	for (const patch of [
		{ balanceAvailable: false },
		{ balance: null },
		{ balance: -1 },
		{ balance: 1.5 },
		{ balance: "12" },
		{ balance: 1e20 },
	]) {
		const service = new CstoaAccount(async () => profile(patch));
		assert.equal((await service.getAccount(runtime())).profile.name, "测试队员");
		const quota = await service.getQuota(runtime());
		assert.equal(quota.supported, false);
		assert.equal(quota.balance, undefined);
		assert.match(quota.reason ?? "", /重试/);
	}
});

test("未登录、明确失效、无访问令牌和 Pi 暂时故障均不请求 OA", async () => {
	let calls = 0;
	const service = new CstoaAccount(async () => {
		calls++;
		return profile();
	});
	assert.equal((await service.getAccount(runtime(false))).signedIn, false);
	assert.equal((await service.getAccount(runtime(), true)).requiresLogin, true);
	assert.equal((await service.getAccount(runtime(true, ""))).requiresLogin, true);
	const failing = runtime();
	failing.getAuth = async () => {
		throw new Error("test-agent-token");
	};
	const account = await service.getAccount(failing);
	assert.equal(account.requiresLogin, false);
	assert.equal(account.signedIn, true);
	assert.match(account.profile.reason ?? "", /重试/);
	assert.equal(account.profile.studentId, null);
	assert.equal(JSON.stringify(account).includes("test-agent-token"), false);
	assert.equal(calls, 0);
});

test("Pi 包装的 CSTOA 明确失效错误要求重新登录，错误内容不进入响应", async () => {
	const current = runtime();
	current.getAuth = async () => {
		throw new Error("OAuth refresh failed", { cause: new CstoaLoginRequired("test-agent-token") });
	};
	const service = new CstoaAccount(async () => {
		throw new Error("不应调用 OA");
	});
	const account = await service.getAccount(current);
	assert.equal(account.signedIn, true);
	assert.equal(account.requiresLogin, true);
	assert.equal((await service.getQuota(current)).requiresLogin, true);
	assert.equal(JSON.stringify(account).includes("test-agent-token"), false);
});

test("HTTP 401/403 和 OA 19008 清空资料并提示重新登录", async () => {
	for (const response of [
		new Response(null, { status: 401 }),
		new Response(null, { status: 403 }),
		Response.json({ result: false, code: 19008 }),
	]) {
		const account = await new CstoaAccount(async () => response).getAccount(runtime());
		assert.equal(account.requiresLogin, true);
		assert.equal(account.profile.name, null);
		assert.equal(account.profile.studentId, null);
	}
});

test("服务异常和无效资料返回短说明，不回显响应正文；失败可立即重试", async () => {
	for (const payload of [
		{ result: true, data: { memberId: "", name: "姓名" } },
		{ result: true, data: { memberId: "id", name: 123 } },
		{ result: false, code: 10005 },
		{ data: { memberId: "id", name: "姓名" } },
	]) {
		const account = await new CstoaAccount(async () => Response.json(payload)).getAccount(runtime());
		assert.equal(account.profile.name, null);
		assert.ok(account.profile.reason);
	}
	for (const status of [404, 405, 500]) {
		let calls = 0;
		const service = new CstoaAccount(async () =>
			++calls === 1 ? new Response("test-agent-token", { status }) : profile(),
		);
		const account = await service.getAccount(runtime());
		assert.equal(account.signedIn, true);
		assert.ok(account.profile.reason);
		assert.equal(JSON.stringify(account).includes("test-agent-token"), false);
		assert.equal((await service.getAccount(runtime())).profile.name, "测试队员");
	}
	const offline = await new CstoaAccount(async () => {
		throw new Error("test-agent-token");
	}).getAccount(runtime());
	assert.match(offline.profile.reason ?? "", /网络/);
	assert.equal(JSON.stringify(offline).includes("test-agent-token"), false);
});

test("缓存有 60 秒上限，显式失效和令牌变化重新取数，服务实例不共享资料", async () => {
	let calls = 0,
		now = 0;
	const fetcher: typeof fetch = async () => {
		calls++;
		return profile();
	};
	const service = new CstoaAccount(fetcher, () => now);
	await service.getAccount(runtime());
	now = 59_999;
	await service.getQuota(runtime());
	assert.equal(calls, 1);
	now = 60_000;
	await service.getAccount(runtime());
	assert.equal(calls, 2);
	service.invalidate();
	await service.getAccount(runtime());
	assert.equal(calls, 3);
	await service.getAccount(runtime(true, "other-token"));
	assert.equal(calls, 4);
	await new CstoaAccount(fetcher).getAccount(runtime());
	assert.equal(calls, 5);
});

test("退出发生在请求期间时，迟到资料不恢复账号，也不写入缓存", async () => {
	let resolve: (response: Response) => void = () => {};
	let started: () => void = () => {};
	const ready = new Promise<void>((done) => {
		started = done;
	});
	const service = new CstoaAccount(async () => {
		started();
		return new Promise<Response>((done) => {
			resolve = done;
		});
	});
	let signedIn = true;
	const current = runtime();
	current.listCredentials = async () => (signedIn ? [{ providerId: "cstoa", type: "oauth" }] : []);
	const pending = service.getAccount(current);
	await ready;
	signedIn = false;
	service.invalidate();
	resolve(profile());
	const account = await pending;
	assert.equal(account.signedIn, false);
	assert.equal(account.profile.name, null);
});

test("外部退出未触发 Web 事件时，迟到响应仍清空身份与额度", async () => {
	const started = deferred<void>();
	const response = deferred<Response>();
	const service = new CstoaAccount(async () => {
		started.resolve();
		return response.promise;
	});
	const current = runtime();
	const pending = service.getAccount(current);
	await started.promise;
	current.listCredentials = async () => [];
	response.resolve(profile());
	const account = await pending;
	assert.equal(account.signedIn, false);
	assert.equal(account.profile.name, null);
	assert.equal((await service.getQuota(current)).supported, false);
});

test("外部切换账号未触发 Web 事件时，迟到响应改读当前凭据", async () => {
	const started = deferred<void>();
	const response = deferred<Response>();
	const service = new CstoaAccount(async (_url, options) => {
		if (new Headers(options?.headers).get("Authorization") === "Bearer first") {
			started.resolve();
			return response.promise;
		}
		return profile({ memberId: "20230002", name: "第二队员", balance: 200 });
	});
	const current = runtime(true, "first");
	const pending = service.getAccount(current);
	await started.promise;
	current.getAuth = runtime(true, "second").getAuth;
	response.resolve(profile());
	assert.equal((await pending).profile.name, "第二队员");
	assert.equal((await service.getQuota(current)).balance, 200);
});

test("不同账号并发时旧请求不覆盖新账号缓存", async () => {
	let resolve: (response: Response) => void = () => {};
	let started: () => void = () => {};
	const ready = new Promise<void>((done) => {
		started = done;
	});
	const service = new CstoaAccount(async (_url, options) => {
		if (new Headers(options?.headers).get("Authorization") === "Bearer first") {
			started();
			return new Promise<Response>((done) => {
				resolve = done;
			});
		}
		return profile({ memberId: "20230002", name: "第二队员" });
	});
	const first = runtime(true, "first");
	const pending = service.getAccount(first);
	await ready;
	const second = runtime(true, "second");
	first.getAuth = second.getAuth;
	assert.equal((await service.getAccount(second)).profile.name, "第二队员");
	resolve(profile());
	assert.equal((await pending).profile.name, "第二队员");
	assert.equal((await service.getAccount(second)).profile.name, "第二队员");
});

test("拒绝明文公网、带凭据或查询参数的 OA 地址，不读取模型自定义端点", async () => {
	let calls = 0;
	const service = new CstoaAccount(async () => {
		calls++;
		return profile();
	});
	try {
		for (const host of [
			"http://public.example",
			"https://user:password@example.test",
			"https://www.cstoa.top/?key=test",
			"invalid",
		]) {
			process.env.CSTOA_OA_HOST = host;
			assert.match((await service.getAccount(runtime())).profile.reason ?? "", /地址无效/);
		}
		assert.equal(calls, 0);
	} finally {
		delete process.env.CSTOA_OA_HOST;
	}
});
