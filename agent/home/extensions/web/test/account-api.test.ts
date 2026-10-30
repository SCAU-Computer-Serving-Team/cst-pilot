import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";
import { CstoaLoginRequired } from "../../oauth/errors.ts";
import { type MockOptions, startMockOa } from "../../oauth/test/mock-oa.ts";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { freePort } from "./browser.ts";
import { testRoot } from "./support.ts";

const home = await mkdtemp(join(await testRoot(), "web-account-"));
const previousAgentDir = process.env.PI_CODING_AGENT_DIR;
const previousHost = process.env.CSTOA_OA_HOST;
process.env.PI_CODING_AGENT_DIR = home;
await writeFile(join(home, "settings.json"), JSON.stringify({ defaultTools: ["read"] }));
const options: MockOptions = {};
const mock = await startMockOa(options);
process.env.CSTOA_OA_HOST = mock.host;
const pool = new WebSessionPool({
	cwd: home,
	agentDir: home,
	sessionDir: join(home, "sessions"),
	withLoader: (load) => load(),
});
const { modelRuntime } = await pool.getServices();
modelRuntime.registerProvider("cstoa", {
	name: "CSTOA API",
	baseUrl: `${mock.host}/api/agent/llm/v1`,
	api: "openai-completions",
	oauth: {
		name: "CSTOA API",
		login: async () => ({ access: "access-1", refresh: "", expires: Date.now() + 3_600_000 }),
		refreshToken: async () => {
			throw new CstoaLoginRequired("测试凭据不可刷新");
		},
		getApiKey: (credentials) => credentials.access,
	},
	models: [
		{
			id: "probe",
			name: "Probe",
			reasoning: false,
			input: ["text"],
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
			contextWindow: 1000,
			maxTokens: 100,
		},
	],
});
const port = await freePort();
const origin = `http://127.0.0.1:${port}`;
const api = createWebApi(pool, home, port);
const server = createWebServer(
	fileURLToPath(new URL("../static/", import.meta.url)),
	port,
	() => ({ sessions: [], stage: "test" }),
	api,
);
await listenWebServer(server, port);
after(async () => {
	await api.close();
	await pool.close();
	server.closeAllConnections();
	await new Promise<void>((resolve) => server.close(() => resolve()));
	await mock.close();
	if (previousAgentDir === undefined) delete process.env.PI_CODING_AGENT_DIR;
	else process.env.PI_CODING_AGENT_DIR = previousAgentDir;
	if (previousHost === undefined) delete process.env.CSTOA_OA_HOST;
	else process.env.CSTOA_OA_HOST = previousHost;
	await rm(home, { recursive: true, force: true });
});
const signIn = (access = "access-1", expires = Date.now() + 3_600_000) =>
	writeFile(join(home, "auth.json"), JSON.stringify({ cstoa: { access, refresh: "", expires, type: "oauth" } }));
const account = (refresh = false) =>
	fetch(`${origin}/api/account${refresh ? "?refresh=1" : ""}`).then((response) => response.json());
const quota = (refresh = false) =>
	fetch(`${origin}/api/quota?provider=cstoa${refresh ? "&refresh=1" : ""}`).then((response) => response.json());

test("账号路由只返回身份；额度路由返回额度点，两者共用一次上游调用", async () => {
	await signIn();
	const before = mock.requests.filter((request) => request.path === "/api/agent/profile").length;
	const [data, balance] = await Promise.all([account(), quota()]);
	assert.equal(data.signedIn, true);
	assert.deepEqual(data.profile, { supported: true, studentId: "20230001", name: "测试队员" });
	assert.equal(data.requiresLogin, false);
	assert.equal("quota" in data, false);
	assert.equal(balance.supported, true);
	assert.equal(balance.balance, 12345);
	assert.equal(balance.unit, "credit");
	assert.equal(JSON.stringify([data, balance]).includes("access-1"), false);
	assert.equal(mock.requests.filter((request) => request.path === "/api/agent/profile").length, before + 1);
	assert.equal((await fetch(`${origin}/api/account`)).headers.get("Cache-Control"), "no-store");
});

test("真实 Pi 请求结束后使 CSTOA 缓存失效，下次查看读取新的额度", async () => {
	options.profile = { balance: 12345 };
	assert.equal((await quota(true)).balance, 12345);
	options.profile = { balance: 10000 };
	assert.equal((await quota()).balance, 12345);
	const slot = await pool.create();
	const selected = modelRuntime.getModel("cstoa", "probe");
	assert.ok(selected);
	await slot.session.setModel(selected);
	assert.equal((await fetch(`${origin}/api/sessions/${slot.id}`)).status, 200);
	await slot.session.prompt("仅测试缓存失效", { source: "rpc" });
	assert.equal(slot.session.messages.at(-1)?.role, "assistant");
	assert.equal((await quota()).balance, 10000);
	assert.ok(mock.requests.some((request) => request.path === "/api/agent/llm/v1/chat/completions"));
	options.profile = undefined;
});

test("显式额度刷新绕过缓存，余额为 0 时照常显示；额度故障保留姓名", async () => {
	options.profile = { balance: 0 };
	assert.equal((await quota(true)).balance, 0);
	options.profile = { balance: null, balanceAvailable: false };
	assert.equal((await quota(true)).supported, false);
	assert.equal((await account()).profile.name, "测试队员");
	options.profile = undefined;
});

test("错误后可立即重试；HTTP 401 清空身份和额度", async () => {
	options.profileStatus = 500;
	assert.ok((await account(true)).profile.reason);
	options.profileStatus = 200;
	assert.equal((await account()).profile.name, "测试队员");
	options.profileStatus = 401;
	const value = await account(true);
	assert.equal(value.requiresLogin, true);
	assert.equal(value.profile.name, null);
	assert.equal((await quota()).requiresLogin, true);
	options.profileStatus = 200;
});

test("外部凭据过期后需要重新登录；退出保留其他服务凭据且不恢复缓存", async () => {
	await signIn("access-1", Date.now() - 10_000);
	const expired = await account();
	assert.equal(expired.signedIn, true);
	assert.equal(expired.requiresLogin, true);
	assert.equal(expired.profile.name, null);
	assert.equal((await quota()).supported, false);
	await signIn();
	await modelRuntime.login("openai", "api_key", { prompt: async () => "test-other-key", notify: () => {} });
	assert.equal((await account()).profile.name, "测试队员");
	const response = await fetch(`${origin}/api/auth/cstoa/logout`, {
		method: "POST",
		headers: {
			Origin: origin,
			"X-CST-Web-Request": "1",
			"Content-Type": "application/json",
			"Idempotency-Key": "account-logout",
		},
		body: "{}",
	});
	assert.equal(response.status, 200);
	assert.equal((await account()).signedIn, false);
	assert.equal((await quota()).supported, false);
	assert.ok((await modelRuntime.listCredentials()).some((entry) => entry.providerId === "openai"));
});

test("凭据在本地更新时账户切换清除旧姓名，不解析令牌载荷", async () => {
	await signIn();
	options.profile = { memberId: "20230001", name: "第一队员" };
	assert.equal((await account(true)).profile.name, "第一队员");
	await signIn("access-2");
	options.profile = { memberId: "20230002", name: "第二队员" };
	const current = await account();
	assert.equal(current.profile.studentId, "20230002");
	assert.equal(current.profile.name, "第二队员");
	await rm(join(home, "auth.json"), { force: true });
	assert.equal((await account()).signedIn, false);
});
