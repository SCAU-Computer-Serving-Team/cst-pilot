/**
 * /api/account 路由测试：mock OA 提供 /api/agent/profile，
 * 验证专属账号的学号、姓名与额度点来自真实调用链。
 */

import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { type AddressInfo, createServer as createNetServer } from "node:net";
import { join } from "node:path";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";
import { startMockOa } from "../../oauth/test/mock-oa.ts";
import { createWebApi } from "../server/api.ts";
import { createWebServer, listenWebServer } from "../server/http.ts";
import { WebSessionPool } from "../server/session/sessions.ts";
import { testRoot } from "./support.ts";

delete process.env.CSTOA_OA_HOST;

const home = await mkdtemp(join(await testRoot(), "web-account-"));
// pi 的凭据存储按 PI_CODING_AGENT_DIR 解析；测试固定到隔离目录，避免读到开发机登录态。
const previousAgentDir = process.env.PI_CODING_AGENT_DIR;
process.env.PI_CODING_AGENT_DIR = home;
await writeFile(join(home, "settings.json"), JSON.stringify({ defaultTools: ["read"] }));
const mock = await startMockOa({
	profile: {
		memberId: "20230001",
		name: "测试队员",
		groupName: "管理员",
		balance: 12345,
		usedQuota: 500,
		requestCount: 3,
	},
});
const pool = new WebSessionPool({
	cwd: home,
	agentDir: home,
	sessionDir: join(home, "sessions"),
	withLoader: (load) => load(),
});
const { modelRuntime } = await pool.getServices();
// baseUrl 指向 mock OA，account 模块据此推导资料接口源站。
modelRuntime.registerProvider("cstoa", {
	name: "CSTOA API",
	baseUrl: `${mock.host}/v1`,
	api: "openai-completions",
	oauth: {
		name: "CSTOA API",
		login: async () => ({ access: "unused", refresh: "", expires: Date.now() + 3_600_000 }),
		refreshToken: async (credentials) => credentials,
		getApiKey: (credentials) => credentials.access,
	},
	// account 模块取第一个模型的 baseUrl 推导 OA 源站。
	models: [
		{
			id: "probe",
			name: "Probe",
			api: "openai-completions",
			baseUrl: `${mock.host}/v1`,
			reasoning: false,
			input: ["text"],
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
			contextWindow: 1000,
			maxTokens: 100,
		},
	],
});
const probe = createNetServer();
await new Promise<void>((resolve) => probe.listen(0, "127.0.0.1", resolve));
const { port } = probe.address() as AddressInfo;
await new Promise<void>((resolve) => probe.close(() => resolve()));
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
	await rm(home, { recursive: true, force: true });
});

const token = (payload: Record<string, unknown>) =>
	`${Buffer.from(JSON.stringify(payload), "utf8").toString("base64url")}.signature`;
const signIn = (access: string) =>
	writeFile(
		join(home, "auth.json"),
		JSON.stringify({ cstoa: { access, refresh: "", expires: Date.now() + 3_600_000, type: "oauth" } }),
	);
const account = () => fetch(`${origin}/api/account`).then((response) => response.json());

test("已登录时返回学号、姓名与剩余额度点", async () => {
	await signIn(token({ mid: "20230001", typ: "agent", exp: Math.floor(Date.now() / 1000) + 3600 }));
	const data = await account();
	assert.equal(data.signedIn, true);
	assert.equal(data.profile.supported, true);
	assert.equal(data.profile.studentId, "20230001");
	assert.equal(data.profile.name, "测试队员");
	assert.equal(data.quota.supported, true);
	assert.equal(data.quota.balance, 12345);
	assert.equal(data.requiresLogin, false);
});

test("凭据过期时标记需要重新登录且不给额度", async () => {
	await signIn(token({ mid: "20230001", typ: "agent", exp: Math.floor(Date.now() / 1000) - 10 }));
	const data = await account();
	assert.equal(data.signedIn, true);
	assert.equal(data.requiresLogin, true);
	assert.equal(data.quota.supported, false);
	assert.equal(data.quota.balance, null);
	assert.equal(data.profile.studentId, "20230001");
});

test("未登录时不给出资料", async () => {
	await rm(join(home, "auth.json"), { force: true });
	const data = await account();
	assert.equal(data.signedIn, false);
	assert.equal(data.profile.supported, false);
	assert.equal(data.quota.supported, false);
});
